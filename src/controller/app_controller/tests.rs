//! AppController integration tests: the full editing workflows driven
//! through the controller API (draw → dimension → carve → exposure →
//! phases → persist/reload), pinned against an in-memory SQLite DB.
use super::*;

/// R3 regression: `activate_shape` with the CURRENT shape as target is a
/// no-op — its early-return must NOT clear transient interaction state.
/// (Switching to a *different* shape does clear it; this pins the no-switch
/// path so the document extraction can't silently change that behaviour.)
#[test]
fn activate_shape_same_target_is_a_noop_for_transient_state() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    assert_eq!(c.document.active, ActiveShape::Outline); // default active shape
                                                         // Seed transient state that a real shape-switch would clear.
    c.pending_cut = Some([ShapeSlot::default(), ShapeSlot::default()]);
    c.document.edit_state.dragging = true;
    // Re-activating the SAME shape must leave it all intact.
    c.activate_shape(ActiveShape::Outline);
    assert!(
        c.pending_cut.is_some(),
        "activate_shape(current) must not clear pending_cut"
    );
    assert!(
        c.document.edit_state.dragging,
        "activate_shape(current) must not reset edit_state"
    );
}

/// The assessor's flow: draw a square, trim a corner, then zero the two
/// remnant cut walls so the cuts snap onto corners — leaving a triangle.
/// Exercises the real controller path: `compute_next_dimension_prompt`
/// picks each axis-aligned remnant wall, and `apply_dimension(0.0)`
/// collapses it (the diagonal cut wall is never offered).
#[test]
fn trim_corner_then_zero_remnants_makes_a_triangle() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());

    // A 10x10 square (5th point is the lift-near-start drag point).
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    // Treat it as fully dimensioned: scale set, both square params known.
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();

    // Trim off the top-right corner: right edge at y=5 to top edge at x=5.
    c.document
        .floor_plan
        .trim_by_stroke(&[Point2f32::new(10.0, 5.0), Point2f32::new(5.0, 10.0)])
        .unwrap();
    assert_eq!(
        c.document.floor_plan.polygons()[0].len(),
        5,
        "corner cut should leave a pentagon"
    );

    // Zero the two remnant cut walls. Each apply_dimension(0.0) collapses
    // one cut vertex; the diagonal is never prompted (non-axis-aligned).
    for step in 0..2 {
        c.compute_next_dimension_prompt();
        assert!(
            c.document.dimensioning.current_prompt.is_some(),
            "expected a promptable remnant wall at step {step}"
        );
        c.apply_dimension(0.0)
            .unwrap_or_else(|e| panic!("zero-collapse failed at step {step}: {e}"));
    }

    let ring = &c.document.floor_plan.polygons()[0];
    assert_eq!(
        ring.len(),
        3,
        "zeroing both remnants should yield a triangle"
    );
    assert!(c.document.floor_plan.validate_simple().is_ok());
    // The triangle is the square minus the cut corner: (0,0),(10,0),(0,10).
    for corner in [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)] {
        assert!(
            ring.iter()
                .any(|p| (p.x - corner.0).abs() < 1e-3 && (p.y - corner.1).abs() < 1e-3),
            "triangle should keep corner {corner:?}"
        );
    }
}

/// Zeroing a regular (orthogonal) wall is rejected — only a trim cut wall
/// can be collapsed. Guards against silently deleting a drawn wall.
#[test]
fn zero_rejected_for_a_regular_wall() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    // Scaled, but no walls dimensioned yet — so a plain orthogonal wall is
    // promptable. (No trim, so no diagonal cut edge anywhere.)
    c.document.dimensioning.scale_set = true;
    c.compute_next_dimension_prompt();
    assert!(c.document.dimensioning.current_prompt.is_some());

    let err = c
        .apply_dimension(0.0)
        .expect_err("0 on a regular wall must be rejected");
    assert!(err.contains("trim cut"), "unexpected error: {err}");
    // Shape untouched.
    assert_eq!(c.document.floor_plan.polygons()[0].len(), 4);
}

/// After a corner trim there are 3 undimensioned segments (2 axis-aligned
/// remnants + the diagonal cut) but only 2 degrees of freedom. The assessor
/// must be able to dimension ANY two — including the diagonal — since in the
/// field they may only be able to measure certain ones. This sets one
/// remnant, then dimensions the diagonal itself, and checks the diagonal
/// ends up the requested length.
#[test]
fn diagonal_cut_wall_is_dimensionable() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();

    // Trim the top-right corner: right edge at y=5 to top edge at x=5.
    c.document
        .floor_plan
        .trim_by_stroke(&[Point2f32::new(10.0, 5.0), Point2f32::new(5.0, 10.0)])
        .unwrap();

    // Set the first prompted (axis-aligned) remnant to a known length.
    c.compute_next_dimension_prompt();
    assert!(c.document.dimensioning.current_prompt.is_some());
    c.apply_dimension(4.0).expect("set first remnant");

    // Now cycle prompts to the diagonal (non-axis-aligned), skipping any
    // remaining axis-aligned remnant, and dimension it to 7.0.
    let mut dimensioned_diagonal = false;
    for _ in 0..5 {
        c.compute_next_dimension_prompt();
        let Some(prompt) = c.document.dimensioning.current_prompt.clone() else {
            break;
        };
        let (ring, edge) = c
            .document
            .floor_plan
            .find_vertex(prompt.wall_start_vertex)
            .unwrap();
        if c.document.floor_plan.wall_axis(ring, edge).is_none() {
            c.apply_dimension(7.0).expect("dimension the diagonal");
            dimensioned_diagonal = true;
            break;
        }
        c.skip_current_dimension();
    }
    assert!(
        dimensioned_diagonal,
        "the diagonal cut wall should be offered and settable"
    );

    // Verify the polygon's one diagonal edge is ~7.0 long.
    let ring = &c.document.floor_plan.polygons()[0];
    let n = ring.len();
    let mut diag_len = None;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        if (a.x - b.x).abs() > 1e-3 && (a.y - b.y).abs() > 1e-3 {
            diag_len = Some(((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt());
        }
    }
    let diag_len = diag_len.expect("a diagonal edge");
    assert!(
        (diag_len - 7.0).abs() < 1e-2,
        "diagonal should be 7.0, got {diag_len}"
    );
    assert!(c.document.floor_plan.validate_simple().is_ok());
}

/// The TrimOrtho tool routes to the orthogonal-polygon trim: a stepped cut
/// path carves a notch (area removed, extra vertices, stays orthogonal) —
/// not the straight line cut the plain Trim tool would attempt.
#[test]
fn trim_ortho_tool_carves_an_orthogonal_notch() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();

    let area = |c: &AppController| -> f32 {
        let r = &c.document.floor_plan.polygons()[0];
        let n = r.len();
        (0..n)
            .map(|i| {
                let (a, b) = (r[i], r[(i + 1) % n]);
                a.x * b.y - b.x * a.y
            })
            .sum::<f32>()
            .abs()
            * 0.5
    };
    let before = area(&c);

    c.set_edit_mode(EditTools::TrimOrtho);
    // Rectangular notch into the top edge (y=10), down to y=7, x in [3,7].
    let notch = vec![
        Point2f32::new(3.0, 10.0),
        Point2f32::new(3.0, 7.0),
        Point2f32::new(7.0, 7.0),
        Point2f32::new(7.0, 10.0),
    ];
    c.apply_trim_stroke(&notch)
        .expect("ortho trim should succeed");

    let after = area(&c);
    assert!(
        after < before,
        "notch should remove area ({after} vs {before})"
    );
    assert!(
        (before - after - 12.0).abs() < 1.0,
        "should remove ~12 (4x3); removed {}",
        before - after
    );
    let ring = &c.document.floor_plan.polygons()[0];
    assert!(ring.len() > 4, "notch adds vertices");
    // Orthogonal notch — no diagonal edge.
    let n = ring.len();
    let has_diag = (0..n).any(|i| {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        (a.x - b.x).abs() > 1e-3 && (a.y - b.y).abs() > 1e-3
    });
    assert!(!has_diag, "an orthogonal notch must have no diagonal edge");
    assert!(c.document.floor_plan.validate_simple().is_ok());
}

fn diag_edges(c: &AppController) -> usize {
    let ring = &c.document.floor_plan.polygons()[0];
    let n = ring.len();
    (0..n)
        .filter(|&i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            (a.x - b.x).abs() > 1e-3 && (a.y - b.y).abs() > 1e-3
        })
        .count()
}

/// Extending a porch flush at a corner leaves that corner mid-wall
/// (collinear). Since only the envelope matters, the controller cleans it
/// up after the splice — so the outline has no spurious collinear vertex.
#[test]
fn extend_porch_at_corner_cleans_the_spurious_vertex() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.set_edit_mode(EditTools::ExtendOrtho);
    // Freehand porch flush at corner (10,10).
    let porch = vec![
        Point2f32::new(10.3, 10.2),
        Point2f32::new(10.1, 14.1),
        Point2f32::new(5.8, 13.9),
        Point2f32::new(6.1, 10.3),
    ];
    c.apply_extend_stroke(&porch).unwrap();

    let ring = &c.document.floor_plan.polygons()[0];
    // The flush corner is gone — the right wall is one straight segment.
    assert!(
        !ring
            .iter()
            .any(|q| (q.x - 10.0).abs() < 1e-2 && (q.y - 10.0).abs() < 1e-2),
        "the flush corner should be cleaned from the envelope, ring={ring:?}"
    );
    assert_eq!(diag_edges(&c), 0, "envelope stays orthogonal");
    assert!(c.document.floor_plan.validate_simple().is_ok());
    // No collinear vertex remains.
    let n = ring.len();
    let collinear = (0..n)
        .filter(|&i| {
            let (pr, cu, nx) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
            let (ux, uy) = (cu.x - pr.x, cu.y - pr.y);
            let (vx, vy) = (nx.x - cu.x, nx.y - cu.y);
            let len = ((ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt()).max(1e-9);
            (ux * vy - uy * vx).abs() / len < 1e-3 && (ux * vx + uy * vy) > 0.0
        })
        .count();
    assert_eq!(collinear, 0, "no spurious collinear vertices remain");
}

fn ring_area(c: &AppController) -> f32 {
    let ring = &c.document.floor_plan.polygons()[0];
    let n = ring.len();
    let mut sum = 0.0_f32;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        sum += a.x * b.y - b.x * a.y;
    }
    (sum * 0.5).abs()
}

/// Dragging a wall in Edit mode must prompt for a distance PERPENDICULAR to
/// the wall: a vertical wall is measured horizontally (axis X), a horizontal
/// wall vertically (axis Y). (User: "moving the E's left wall showed me a
/// vertical dimension; it should be horizontal.")
#[test]
fn dragging_a_wall_measures_perpendicular_to_it() {
    let make = || {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        // The drag prompt only appears once the plan is dimensioned.
        c.document.dimensioning.scale_set = true;
        c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
        c.set_edit_mode(EditTools::Move);
        c
    };

    // Left (vertical) wall at x=0 → measured along X.
    let mut c = make();
    assert!(c.edit_mouse_down(0.0, 5.0, 1.0), "should hit the left wall");
    c.edit_mouse_drag(0.5, 5.0); // nudge it horizontally
    c.edit_mouse_up().unwrap();
    let p = c
        .edit_drag_prompt
        .as_ref()
        .expect("a measurement prompt after dragging a wall");
    assert_eq!(
        p.axis,
        ParameterAxis::X,
        "a vertical wall is measured horizontally"
    );

    // Bottom (horizontal) wall at y=0 → measured along Y.
    let mut c = make();
    assert!(
        c.edit_mouse_down(5.0, 0.0, 1.0),
        "should hit the bottom wall"
    );
    c.edit_mouse_drag(5.0, 0.5);
    c.edit_mouse_up().unwrap();
    let p = c
        .edit_drag_prompt
        .as_ref()
        .expect("a measurement prompt after dragging a wall");
    assert_eq!(
        p.axis,
        ParameterAxis::Y,
        "a horizontal wall is measured vertically"
    );
}

/// Drawing a living area inside a dimensioned storey: it inherits the
/// storey's scale (no recalibration), becomes the active shape, is stored,
/// and the storey outline is untouched when you switch back.
#[test]
fn living_area_inherits_scale_is_stored_and_leaves_outline_intact() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    c.document.dimensioning.scale_set = true; // storey is dimensioned (metres)
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    let outline_verts = c.document.floor_plan.polygons()[0].len();

    // Enter living-area mode and draw a sub-area inside the storey.
    c.set_living_area_mode(true);
    assert!(c.living_area_mode());
    let la: Vec<(f32, f32)> = vec![(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)];
    c.apply_living_area_stroke_event(&la);

    assert_eq!(c.living_area_count(), 1, "the living area is stored");
    assert_eq!(
        c.active_living_area_index(),
        Some(0),
        "it's the active shape"
    );
    // Inherits the storey scale — no first-dimension calibration step.
    assert!(
        c.dimensioning_scale_set(),
        "a living area inside a dimensioned storey inherits its scale"
    );
    assert!(
        c.document.floor_plan.polygons()[0].len() >= 4,
        "a sub-area was drawn"
    );
    assert_eq!(c.living_area_polygons().len(), 1, "renderable");

    // Switch back to the outline: it's restored exactly, area still stored.
    c.set_living_area_mode(false);
    assert_eq!(c.active_living_area_index(), None);
    assert_eq!(
        c.document.floor_plan.polygons()[0].len(),
        outline_verts,
        "the storey outline is untouched by drawing a living area"
    );
    assert_eq!(c.living_area_count(), 1, "the living area is still kept");
}

/// A fully-dimensioned living area is persisted as resolved geometry and
/// comes back on reopen (alongside the outline). Un-dimensioned areas are
/// NOT written to the DB — only finalised geometry is.
#[test]
fn living_areas_survive_save_and_reopen() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let ber = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();

    c.set_living_area_mode(true);
    c.apply_living_area_stroke_event(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)]);
    assert_eq!(c.living_area_count(), 1);
    // Finalise the living area (mark every wall dimensioned) so it's written
    // to the DB — only finalised geometry is persisted.
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();

    // Reopen from the DB (same connection): the living area is restored.
    c.open_ber(&ber.id).unwrap();
    assert_eq!(
        c.living_area_count(),
        1,
        "the living area survives save + reopen"
    );
    assert_eq!(
        c.active_living_area_index(),
        None,
        "reopen starts on the storey outline"
    );
    assert_eq!(
        c.living_area_polygons().len(),
        1,
        "still renderable after reopen"
    );
}

/// An un-dimensioned outline is still in pixels, not metres, so it is NOT
/// written to the DB — only a finalised (scale-set, fully dimensioned)
/// outline persists. (The in-memory copy keeps it, for rendering.)
#[test]
fn undimensioned_outline_is_not_persisted() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let ber = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    // Draw an outline but leave it un-dimensioned (scale never set).
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(
        &[
            Point2f32::new(0., 0.),
            Point2f32::new(10., 0.),
            Point2f32::new(10., 10.),
            Point2f32::new(0., 10.),
            Point2f32::new(0.2, 0.2),
        ],
        0.5,
        true,
    )
    .unwrap();
    c.commit_to_ber().unwrap();
    c.flush_if_dirty().unwrap();

    // The DURABLE record holds no geometry (the outline wasn't finalised, so
    // it's filtered out of `data_json`). The editing-layer snapshot recovers
    // the in-progress sketch separately on reopen.
    let durable = BerRepository::new(&c.conn).load(&ber.id).unwrap();
    assert!(
        durable
            .data
            .storeys
            .first()
            .map_or(true, |s| s.floors.regions.is_empty()),
        "an un-dimensioned outline isn't written into the durable BER record"
    );
    c.open_ber(&ber.id).unwrap();
    assert!(
        !c.document.outline.floor_plan.get_points().is_empty(),
        "...but the snapshot recovers the in-progress outline on reopen"
    );
}

/// A living area drawn but not dimensioned is excluded from the DB write
/// (kept in memory); the dimensioned outline around it still persists.
#[test]
fn undimensioned_living_area_is_not_persisted() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let ber = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(
        &[
            Point2f32::new(0., 0.),
            Point2f32::new(10., 0.),
            Point2f32::new(10., 10.),
            Point2f32::new(0., 10.),
            Point2f32::new(0.2, 0.2),
        ],
        0.5,
        true,
    )
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();

    c.set_living_area_mode(true);
    c.apply_living_area_stroke_event(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)]);
    assert_eq!(c.living_area_count(), 1, "drawn (in memory)");
    c.flush_if_dirty().unwrap();

    // The DURABLE record holds no living area (un-dimensioned → filtered),
    // though the dimensioned outline IS written.
    let durable = BerRepository::new(&c.conn).load(&ber.id).unwrap();
    let storey = durable.data.storeys.first().expect("storey persisted");
    assert!(
        storey.living_areas.regions.is_empty(),
        "un-dimensioned living area isn't written into the durable BER record"
    );
    assert!(
        !storey.floors.regions.is_empty(),
        "but the dimensioned outline is"
    );
}

/// Reproduces the reported "stuck dimensioning a living area" bug: a
/// free-standing interior square living area should accept its first
/// dimension (it inherits the storey scale, so dimension #1 sets a wall
/// directly — no calibration step).
#[test]
fn dimensioning_a_living_area_square_accepts_the_first_wall() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let ber = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    let _ = ber;
    // 2m x 2m storey outline, already in metres + fully dimensioned.
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(2.0, 0.0),
        Point2f32::new(2.0, 2.0),
        Point2f32::new(0.0, 2.0),
        Point2f32::new(0.05, 0.05),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.1, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();

    c.set_living_area_mode(true);
    // ~0.9m square, interior (doesn't reach the walls).
    c.apply_living_area_stroke_event(&[
        (0.5, 0.5),
        (1.4, 0.5),
        (1.4, 1.4),
        (0.5, 1.4),
        (0.55, 0.55),
    ]);
    assert_eq!(c.living_area_count(), 1, "living area drawn");
    assert!(
        c.dimensioning_active(),
        "it should be prompting to dimension the new area"
    );
    let r = c.apply_dimension(0.9);
    assert!(r.is_ok(), "first living-area dimension should apply: {r:?}");
}

/// A construction-phase area must be DIMENSIONED before its phase is
/// assigned (the exact boundary decides which surfaces are in the phase). So:
/// carve → dimension the drawn walls → only THEN the phase picker → assign
/// stores the dimensioned ring and returns to the outline.
#[test]
fn phase_area_is_dimensioned_before_its_phase_is_assigned() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    // 2x2 dimensioned storey.
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(2.0, 0.0),
        Point2f32::new(2.0, 2.0),
        Point2f32::new(0.0, 2.0),
        Point2f32::new(0.05, 0.05),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.1, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.sync_active(); // flush into the outline slot so phase carves against it

    // Define a construction phase.
    let mut phases = ConstructionPhases::default();
    phases.set_year(ConstructionPhase::Extension1, 2008);
    c.set_construction_phases(phases);

    // Draw an interior phase area (a ~0.9m square that doesn't reach a wall).
    c.set_phase_mode(true);
    c.apply_phase_region_stroke_event(&[
        (0.5, 0.5),
        (1.4, 0.5),
        (1.4, 1.4),
        (0.5, 1.4),
        (0.55, 0.55),
    ]);

    // It must prompt for dimensions — NOT be immediately pickable.
    assert!(
        c.dimensioning_active(),
        "phase area prompts to be dimensioned"
    );
    assert!(!c.phase_pick_pending(), "no phase pick until dimensioned");

    // Dimension every drawn wall.
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        c.apply_dimension(0.9).expect("dimension applies");
        guard += 1;
    }
    // Lengths done — but an interior area's POSITION is still approximate,
    // so the position prompts gate the picker until entered or skipped.
    assert!(!c.phase_pick_pending(), "no phase pick until positioned");
    skip_position_prompts(&mut c);
    assert!(c.phase_pick_pending(), "now the phase picker shows");
    assert!(
        c.pending_phase_polygon().len() >= 4,
        "dimensioned ring available: {:?}",
        c.pending_phase_polygon()
    );

    // Assign the phase → stored region, back to the outline.
    c.assign_phase_to_pending(ConstructionPhase::Extension1);
    assert!(!c.phase_pick_pending(), "pick consumed");
    assert_eq!(
        c.phase_region_polygons().len(),
        1,
        "the dimensioned phase area is stored: {:?}",
        c.phase_region_polygons()
    );
    // And the outline is the active buffer again, intact (the 2x2 square).
    assert_eq!(
        c.document.floor_plan.get_points().len(),
        4,
        "outline restored as the live buffer: {:?}",
        c.document.floor_plan.get_points()
    );
}

/// The reported case: a living area drawn as a square with a nook, in a
/// dimensioned (metre-scale) storey. The orthogonalizer's coordinate-merge
/// is scale-relative, so the nook is PRESERVED (it isn't merged away by a
/// pixel-scale absolute tolerance), and the resulting shape is a valid
/// polygon that dimensions cleanly — not the old "stuck" zero-length-wall
/// case.
#[test]
fn living_area_keeps_a_nook_and_dimensions_cleanly() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(2.6, 0.0),
        Point2f32::new(2.6, 2.0),
        Point2f32::new(0.0, 2.0),
        Point2f32::new(0.05, 0.05),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.1, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();

    c.set_living_area_mode(true);
    // ~0.9m square with a nook off the right side, offset into the storey.
    c.apply_living_area_stroke_event(&[
        (1.1, 0.5),
        (2.0, 0.5),
        (2.0, 0.8),
        (2.07, 0.8), // nook out
        (2.07, 0.95),
        (2.0, 0.95),
        (2.0, 1.4),
        (1.1, 1.4),
        (1.12, 0.52),
    ]);
    assert_eq!(c.living_area_count(), 1);
    let poly = c.living_area_polygons()[0].clone();
    assert!(
        poly.len() > 4,
        "the nook is preserved (not flattened by a pixel-scale tolerance): {poly:?}"
    );
    assert!(
        poly.iter().any(|(x, _)| (*x - 2.07).abs() < 0.02),
        "the nook's outer edge survives: {poly:?}"
    );
    assert!(c.dimensioning_active(), "prompts to dimension the area");
    let r = c.apply_dimension(0.93);
    assert!(
        r.is_ok(),
        "the living area dimensions without rejection: {r:?}"
    );
}

/// Drawing a living area against the house walls (an OPEN path whose ends
/// reach the outline) produces a corner room that shares those walls — this
/// is the path that previously drew nothing (the stroke was force-closed
/// into a degenerate loop and rejected). Entering living-area mode also arms
/// the draw tool so the stroke routes to a new area.
#[test]
fn living_area_against_a_corner_shares_house_walls_and_renders() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();

    c.set_living_area_mode(true);
    assert_eq!(
        c.edit_mode(),
        EditTools::DrawPerpWalls,
        "entering living-area mode arms the draw tool"
    );
    // Open L against the left + bottom walls: only the 2 NEW interior walls.
    // An edge-to-edge stroke divides the storey, so it offers both regions to
    // pick — tap inside the corner to keep that one.
    c.apply_living_area_stroke_event(&[(0.0, 4.0), (4.0, 4.0), (4.0, 0.0)]);
    assert!(c.cut_pick_pending(), "the dividing L offers a region pick");
    c.choose_cut_region(2.0, 2.0);

    assert_eq!(
        c.living_area_count(),
        1,
        "the corner living area is created"
    );
    let polys = c.living_area_polygons();
    assert_eq!(polys.len(), 1, "and it's renderable (was drawing nothing)");
    assert_eq!(polys[0].len(), 4, "quad: 2 drawn + 2 shared house walls");
    assert!(
        polys[0]
            .iter()
            .any(|(x, y)| x.abs() < 0.05 && y.abs() < 0.05),
        "shares the storey corner (0,0): {:?}",
        polys[0]
    );
}

/// A shared-wall (carved) living area must keep its FULL shape when saved —
/// including the vertices that lie on the house walls (the shared corner and
/// the snap points) — so it round-trips through the DB intact rather than
/// losing the house-side boundary.
#[test]
fn shared_wall_living_area_keeps_house_vertices_on_save() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let ber = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();

    c.set_living_area_mode(true);
    // Corner room: only the two interior walls drawn; the left + bottom house
    // walls (and the (0,0) corner) are filled in. The edge-to-edge stroke
    // offers a region pick — tap inside the corner.
    c.apply_living_area_stroke_event(&[(0.0, 4.0), (4.0, 4.0), (4.0, 0.0)]);
    c.choose_cut_region(2.0, 2.0);
    let before = c.living_area_polygons()[0].clone();
    assert_eq!(before.len(), 4, "full closed shape before save: {before:?}");
    // Finalise it so it's persisted (only finalised geometry is written).
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();

    // Save + reopen from the DB.
    c.open_ber(&ber.id).unwrap();
    let after = c.living_area_polygons()[0].clone();
    assert_eq!(
        after.len(),
        4,
        "the full shape (incl. house-wall vertices) survives save+reopen: {after:?}"
    );
    // The shared corner (0,0) and the two snap points on the house walls
    // are all still present.
    for want in [(0.0, 0.0), (0.0, 4.0), (4.0, 0.0)] {
        assert!(
            after
                .iter()
                .any(|(x, y)| (x - want.0).abs() < 0.1 && (y - want.1).abs() < 0.1),
            "house-wall vertex {want:?} retained: {after:?}"
        );
    }
}

/// Drawing a single cut line splits the storey into two candidate regions
/// and waits for the user to tap one — which then becomes the living area.
/// Tapping the left side keeps the left region; tapping outside cancels.
#[test]
fn cut_line_offers_both_regions_then_keeps_the_tapped_one() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    // 10m x 6m storey, in metres + dimensioned.
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(
        &[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 6.0),
            Point2f32::new(0.0, 6.0),
            Point2f32::new(0.1, 0.1),
        ],
        0.2,
        true,
    )
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();

    c.set_living_area_mode(true);
    // A vertical cut at x~3 (drawn as a noisy near-straight stroke).
    c.apply_living_area_stroke_event(&[(3.0, 0.05), (3.0, 2.0), (3.0, 5.95)]);
    assert!(c.cut_pick_pending(), "the cut is awaiting a pick");
    assert_eq!(c.living_area_count(), 0, "nothing committed until picked");
    assert_eq!(c.pending_cut_regions().len(), 2, "two candidate regions");

    // Tap outside both regions → cancels (no area, no pending).
    c.choose_cut_region(50.0, 50.0);
    assert!(!c.cut_pick_pending());
    assert_eq!(c.living_area_count(), 0);

    // Redraw the cut and tap the LEFT region (x<3) → keeps the left ~3x6=18.
    c.apply_living_area_stroke_event(&[(3.0, 0.05), (3.0, 2.0), (3.0, 5.95)]);
    assert!(c.cut_pick_pending());
    c.choose_cut_region(1.0, 3.0);
    assert!(!c.cut_pick_pending());
    assert_eq!(
        c.living_area_count(),
        1,
        "the tapped region became the area"
    );
    let poly = c.living_area_polygons()[0].clone();
    let area: f64 = {
        let n = poly.len();
        let mut s = 0.0;
        for i in 0..n {
            let (x1, y1) = poly[i];
            let (x2, y2) = poly[(i + 1) % n];
            s += x1 * y2 - x2 * y1;
        }
        (s / 2.0).abs()
    };
    assert!(
        (area - 18.0).abs() < 1.0,
        "kept the left region (~18 m²): {area}"
    );
}

/// Walls of a living area that lie on the storey are PINNED (their
/// parameter pre-known), so dimensioning only asks about the walls the user
/// drew. A left cut-region shares the storey's left/top/bottom walls; only
/// the cut wall's position remains to set.
#[test]
fn snapped_walls_are_pinned_so_only_drawn_walls_dimension() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(
        &[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 6.0),
            Point2f32::new(0.0, 6.0),
            Point2f32::new(0.1, 0.1),
        ],
        0.2,
        true,
    )
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();

    c.set_living_area_mode(true);
    c.apply_living_area_stroke_event(&[(3.0, 0.05), (3.0, 2.0), (3.0, 5.95)]);
    c.choose_cut_region(1.0, 3.0); // left region: shares left/top/bottom house walls
    assert_eq!(c.living_area_count(), 1);

    let total = c.document.floor_plan.all_active_params().len();
    let known = c.document.dimensioning.known_params.len();
    assert!(
        known >= 1,
        "a shared wall's parameter is pre-known (total={total}, known={known})"
    );
    assert_eq!(
        total - known,
        1,
        "only the cut wall's parameter remains to dimension (total={total}, known={known})"
    );
}

/// Helper: a 10×10 dimensioned storey with a BER record, ready for exposure
/// marking. Returns the controller.
#[cfg(test)]
pub(super) fn dimensioned_square_controller() -> AppController {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(
        &[
            p(0., 0.),
            p(10., 0.),
            p(10., 10.),
            p(0., 10.),
            p(0.05, 0.05),
        ],
        0.1,
        true,
    )
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
    c
}

/// Accept the drawn position for every pending position prompt (skip them
/// all), so a test can drive a carved area past the position loop to its
/// phase/level pick.
fn skip_position_prompts(c: &mut AppController) {
    let mut guard = 0;
    while c.document.dimensioning.current_position_prompt.is_some() && guard < 8 {
        c.skip_position_dimension().unwrap();
        guard += 1;
    }
    assert!(guard < 8, "position-prompt loop didn't terminate");
}

/// Drive the wall-length loop of a freshly carved 4×4 interior area (drawn at
/// (2,2)–(6,6) in the 10×10 storey) to completion.
fn complete_interior_length_loop(c: &mut AppController) {
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        c.apply_dimension(4.0).expect("dimension applies");
        guard += 1;
    }
    assert!(guard < 12, "length loop didn't terminate");
}

fn buffer_min_x(c: &AppController) -> f32 {
    c.document
        .floor_plan
        .get_points()
        .iter()
        .map(|p| p.x)
        .fold(f32::INFINITY, f32::min)
}

/// Slice-3 gate, phase flavour: a free-standing (fully interior) phase area
/// finishes its wall-length loop and lands in the POSITION loop — the phase
/// pick stays gated until both axes are entered. The UI data resolves its two
/// VertexIds against the right plans (area wall → live buffer, reference wall
/// → storey outline).
#[test]
fn interior_phase_area_position_prompts_gate_the_phase_pick() {
    let mut c = dimensioned_square_controller();
    c.set_phase_mode(true);
    // Interior 4x4 at (2,2)–(6,6): no outline contact → both axes free.
    c.apply_phase_region_stroke_event(&[
        (2.0, 2.0),
        (6.0, 2.0),
        (6.0, 6.0),
        (2.0, 6.0),
        (2.1, 2.1),
    ]);
    complete_interior_length_loop(&mut c);

    // Lengths done; the position prompt (not the phase pick) is up.
    assert!(
        c.document.dimensioning.current_position_prompt.is_some(),
        "position prompt after the length loop"
    );
    assert!(!c.phase_pick_pending(), "phase pick gated by position");

    // UI data: the area wall comes from the LIVE buffer (left wall at x≈2),
    // the reference wall from the storey OUTLINE (its x=0 wall) — two
    // different VertexId namespaces.
    let ui = c.position_prompt_for_ui().expect("ui data for the prompt");
    assert_eq!(ui.axis, ParameterAxis::X);
    assert_eq!(ui.option_count, 2, "outline refs x=0 and x=10");
    assert_eq!(ui.option_idx, 0);
    // The carved area's left wall sits at x=2.1 (the stroke's lift point
    // becomes the anchored Constant), so the drawn distance from x=0 is 2.1.
    assert!((ui.current_distance - 2.1).abs() < 1e-3, "drawn ~2.1 m off");
    assert!(
        ui.reference_wall.wall_a.0.abs() < 1e-3 && ui.reference_wall.wall_b.0.abs() < 1e-3,
        "reference is the outline's x=0 wall: {:?}",
        ui.reference_wall
    );
    assert!(
        (ui.area_wall.wall_a.0 - 2.1).abs() < 1e-3 && (ui.area_wall.wall_b.0 - 2.1).abs() < 1e-3,
        "area wall is the buffer's x≈2.1 wall: {:?}",
        ui.area_wall
    );

    // Enter X then Y → only then the phase pick opens.
    c.apply_position_dimension(3.0).expect("X position applies");
    assert!(!c.phase_pick_pending(), "Y still pending");
    assert!((buffer_min_x(&c) - 3.0).abs() < 1e-3, "area moved to x=3");
    c.apply_position_dimension(2.0).expect("Y position applies");
    assert!(
        c.document.dimensioning.current_position_prompt.is_none(),
        "both axes entered"
    );
    assert!(c.phase_pick_pending(), "phase pick now open");
}

/// Slice-3 gate, step flavour: same gating for a step area, resolved by
/// SKIPPING each position prompt (accept the drawn position).
#[test]
fn interior_step_area_position_skip_opens_the_level_entry() {
    let mut c = dimensioned_square_controller();
    c.set_step_mode(true);
    c.apply_step_region_stroke_event(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)]);
    complete_interior_length_loop(&mut c);

    assert!(
        c.document.dimensioning.current_position_prompt.is_some(),
        "position prompt after the length loop"
    );
    assert!(!c.step_pick_pending(), "level entry gated by position");
    let before = c.document.floor_plan.get_points();

    c.skip_position_dimension().unwrap(); // X — accept as drawn
    assert!(!c.step_pick_pending(), "Y still pending");
    c.skip_position_dimension().unwrap(); // Y — accept as drawn
    assert!(
        c.document.dimensioning.current_position_prompt.is_none(),
        "both axes resolved"
    );
    assert!(c.step_pick_pending(), "level entry now open");
    assert_eq!(
        c.document.floor_plan.get_points(),
        before,
        "skip moves nothing"
    );
}

/// Undo after `apply_position_dimension` restores both the geometry and the
/// prompt progress: the area returns to its drawn position and the same
/// position prompt is asked again.
#[test]
fn undo_after_apply_position_dimension_restores_geometry_and_prompt() {
    let mut c = dimensioned_square_controller();
    c.set_phase_mode(true);
    c.apply_phase_region_stroke_event(&[
        (2.0, 2.0),
        (6.0, 2.0),
        (6.0, 6.0),
        (2.0, 6.0),
        (2.1, 2.1),
    ]);
    complete_interior_length_loop(&mut c);
    let before = c.document.floor_plan.get_points();
    assert_eq!(
        c.document
            .dimensioning
            .current_position_prompt
            .as_ref()
            .map(|p| p.axis),
        Some(ParameterAxis::X)
    );

    c.apply_position_dimension(3.0).expect("X position applies");
    assert!((buffer_min_x(&c) - 3.0).abs() < 1e-3, "area moved to x=3");
    assert_eq!(
        c.document
            .dimensioning
            .current_position_prompt
            .as_ref()
            .map(|p| p.axis),
        Some(ParameterAxis::Y),
        "advanced to the Y prompt"
    );

    c.undo().expect("undo");
    assert_eq!(
        c.document.floor_plan.get_points(),
        before,
        "geometry restored to the drawn position"
    );
    assert_eq!(
        c.document
            .dimensioning
            .current_position_prompt
            .as_ref()
            .map(|p| p.axis),
        Some(ParameterAxis::X),
        "the X position prompt returns"
    );
    assert!(!c.phase_pick_pending(), "phase pick gated again");
}

/// Living areas persist as resolved polygons in `Storey.living_areas` (part
/// of the `Ber` struct), NOT in `sketch_state`, and are rebuilt from there
/// on reload.
#[test]
fn living_areas_persist_in_ber_and_reload() {
    let mut c = dimensioned_square_controller();
    c.set_living_area_mode(true);
    c.apply_living_area_stroke_event(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)]);
    assert_eq!(c.living_area_count(), 1, "one living area drawn");

    c.materialize(); // edits defer the derive; bring current_record current
    let storey = c.current_record.as_ref().unwrap().data.storeys[0].clone();
    assert_eq!(
        storey.living_areas.regions.len(),
        1,
        "persisted into Storey.living_areas"
    );

    // Reload from the full storey → the living area is rebuilt.
    c.load_storey_into_editor(Some(&storey));
    assert_eq!(c.living_area_count(), 1, "rebuilt on reload");

    // Source is Storey.living_areas, not sketch_state: clear the regions but
    // keep sketch_state → no living areas come back.
    let mut storey_no_la = storey.clone();
    storey_no_la.living_areas = FloorRegions::default();
    c.load_storey_into_editor(Some(&storey_no_la));
    assert_eq!(
        c.living_area_count(),
        0,
        "sketch_state doesn't carry living areas"
    );
}

/// Deleting a living area removes it from the editor and the `Ber` struct.
#[test]
fn delete_living_area_removes_it() {
    let mut c = dimensioned_square_controller();
    c.set_living_area_mode(true);
    c.apply_living_area_stroke_event(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)]);
    assert_eq!(c.living_area_count(), 1);

    c.delete_living_area(0);
    assert_eq!(c.living_area_count(), 0, "removed from the editor");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(
        storey.living_areas.regions.len(),
        0,
        "removed from the Ber struct"
    );
}

/// Tapping a wall in exposure mode selects it (whole-wall selection);
/// assigning a type stores a full-length section AND derives a BER
/// `WallRegion` (vertical plane, default ceiling height) of that exposure.
/// Unmarked walls derive `Unset` regions — present, just not classified.
#[test]
fn marking_a_wall_sets_exposure_and_derives_a_ber_wall() {
    let mut c = dimensioned_square_controller();
    let walls = c.document.outline.floor_plan.perimeter_walls();
    assert!(walls.len() >= 4, "square has 4 perimeter walls");
    let (vid0, a0, b0) = walls[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);

    c.set_exposure_mode(true);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    assert_eq!(c.selected_exposure_wall, Some(vid0), "tap selects the wall");
    c.set_wall_exposure(WallExposure::SemiExposed);
    let secs = c
        .document
        .wall_sections
        .get(&vid0)
        .expect("wall0 has a section");
    assert_eq!(secs.len(), 1);
    assert_eq!(secs[0].exposure, WallExposure::SemiExposed);

    // The FULL perimeter derives: the marked wall carries its exposure, the
    // three unmarked walls come through as Unset regions.
    c.materialize(); // edits defer the derive; bring current_record current
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(
        storey.walls.regions.len(),
        walls.len(),
        "every wall derives"
    );
    assert_eq!(
        storey
            .walls
            .regions
            .iter()
            .filter(|r| r.exposure == WallExposure::Unset)
            .count(),
        walls.len() - 1,
        "unmarked walls are Unset"
    );
    let w0 = storey
        .walls
        .regions
        .iter()
        .find(|r| r.exposure == WallExposure::SemiExposed)
        .expect("the marked wall's region");
    assert_eq!(w0.polygon.exterior.len(), 4, "vertical plane = 4 vertices");
    assert!(w0.polygon.exterior.iter().any(|q| q.z == 0.0));
    // 2.4 = document::resolve::PROVISIONAL_WALL_EXTRUSION_M (pub(crate) there):
    // no ceiling height is set, so walls extrude to the provisional placeholder.
    assert!(w0.polygon.exterior.iter().any(|q| (q.z - 2.4).abs() < 1e-9));
}

/// A section over PART of a wall derives several regions: the marked range
/// plus the un-marked remainder as Unset gaps, sub-segment lengths summing
/// to the wall length.
#[test]
fn mid_wall_section_splits_into_multiple_regions() {
    let mut c = dimensioned_square_controller();
    let (vid0, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
    let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();

    c.set_exposure_mode(true);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    assert_eq!(c.selected_exposure_wall, Some(vid0));
    // Mark the middle [0.2L, 0.5L] semi-exposed (typed-range path).
    c.set_exposure_section_range(len * 0.2, len * 0.5);
    c.set_wall_exposure(WallExposure::SemiExposed);
    // ONLY the painted section is stored — the leftovers stay unset (they
    // exist as Unset ranges in the derived coverage, never as sections).
    let secs = c
        .document
        .wall_sections
        .get(&vid0)
        .expect("wall0 has sections");
    assert_eq!(secs.len(), 1);
    assert_eq!(secs[0].exposure, WallExposure::SemiExposed);

    // wall0 is derived first → its 3 coverage ranges (Unset gap, the mark,
    // Unset gap) are regions[0..3]; the other 3 walls follow as whole-wall
    // Unset regions.
    c.materialize(); // edits defer the derive; bring current_record current
    let r = &c.current_record.as_ref().unwrap().data.storeys[0]
        .walls
        .regions;
    assert_eq!(r.len(), 6);
    assert_eq!(r[0].exposure, WallExposure::Unset);
    assert_eq!(r[1].exposure, WallExposure::SemiExposed);
    assert_eq!(r[2].exposure, WallExposure::Unset);
    let base = |reg: &ber_model::ber::structural::WallRegion| {
        let e = &reg.polygon.exterior;
        ((e[1].x - e[0].x).powi(2) + (e[1].y - e[0].y).powi(2)).sqrt()
    };
    let l = len as f64;
    assert!((base(&r[0]) - l * 0.2).abs() < 0.02, "{}", base(&r[0]));
    assert!((base(&r[1]) - l * 0.3).abs() < 0.02, "{}", base(&r[1]));
    assert!((base(&r[2]) - l * 0.5).abs() < 0.02, "{}", base(&r[2]));
}

/// Splitting a wall preserves its fabric properties: each piece inherits the
/// construction of the region it was carved from; only exposure changes.
#[test]
fn wall_construction_is_preserved_when_splitting() {
    use ber_model::ber::structural::WallConstructionType;
    let mut c = dimensioned_square_controller();
    let (_, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
    let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();

    c.set_exposure_mode(true);
    // Mark wall0 whole so it derives to a region we can attach a construction to.
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_wall_exposure(WallExposure::Exposed);
    // Assign wall0 a known construction (type + make-up) via the durable store
    // (the parametric home that survives the re-derivation).
    c.materialize(); // derive wall0's single region first
    let wall0 = c.current_record.as_ref().unwrap().data.storeys[0]
        .walls
        .regions[0]
        .id;
    c.set_wall_construction_for(
        &[wall0],
        WallConstructionType::CavityWall,
        Default::default(),
        Default::default(),
    );

    // Exposure-split wall0: mark the middle semi-exposed → 3 regions.
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_exposure_section_range(len * 0.2, len * 0.5);
    c.set_wall_exposure(WallExposure::SemiExposed);
    c.materialize();

    let record = c.current_record.as_ref().unwrap();
    let r = &record.data.storeys[0].walls.regions;
    // The three exposure-pieces of wall0 all carry the assigned construction,
    // re-derived from the store keyed by wall0's SurfaceRef (not a stable id).
    for i in 0..3 {
        let wc = record.data.wall_construction(r[i].construction_id).unwrap();
        assert_eq!(
            wc.construction_type,
            WallConstructionType::CavityWall,
            "piece {i} keeps the assigned construction"
        );
    }
    assert_eq!(r[1].exposure, WallExposure::SemiExposed, "middle is semi");
}

/// A non-default WALL construction carried on a resolved region is RECOVERED
/// into the durable SurfaceRef store on load (the snapshot-less / legacy path),
/// so the first fresh-id re-resolve doesn't reset it to the default. Mirror of
/// the floor/ceiling seeding — regression for the missing walls branch in
/// `seed_construction_stores_from_record`.
#[test]
fn wall_construction_seeded_from_record_on_load() {
    use ber_model::ber::structural::WallConstructionType;
    let mut c = dimensioned_square_controller();
    // Mark a wall so it derives to a region, then assign it a known construction.
    let (_, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
    c.set_exposure_mode(true);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_wall_exposure(WallExposure::Exposed);
    c.materialize();
    let wall0 = c.current_record.as_ref().unwrap().data.storeys[0]
        .walls
        .regions[0]
        .id;
    c.set_wall_construction_for(
        &[wall0],
        WallConstructionType::CavityWall,
        Default::default(),
        Default::default(),
    );
    c.materialize();

    // Simulate a snapshot-less reload: the resolved record keeps the construction
    // on its wall region, but the editing store is rebuilt EMPTY (`from_storey`
    // doesn't reconstruct it). The seeding fallback must recover it.
    c.document.storeys[0].wall_constructions.clear();
    c.seed_construction_stores_from_record();
    assert!(
        !c.document.storeys[0].wall_constructions.is_empty(),
        "the wall construction was seeded back into the durable SurfaceRef store"
    );

    // End to end: it survives the next re-resolve (would reset to default
    // without the seed, since wall ids are freshly minted every resolve).
    c.commit_to_ber().unwrap();
    let rec = c.current_record.as_ref().unwrap();
    let kept = rec.data.storeys[0].walls.regions.iter().any(|r| {
        rec.data
            .wall_construction(r.construction_id)
            .is_some_and(|wc| wc.construction_type == WallConstructionType::CavityWall)
    });
    assert!(kept, "the seeded wall construction survives the re-resolve");
}

/// A FLOOR construction assigned via the durable centroid-keyed store survives
/// the re-derivation (the area analogue of the wall-construction test): the
/// floor region re-resolves to the assigned `(type, make-up)`.
#[test]
fn floor_construction_survives_rederive() {
    use ber_model::ber::structural::FloorConstructionType;
    let mut c = dimensioned_square_controller();
    c.materialize(); // derive the single floor region
    let floor0 = c.current_record.as_ref().unwrap().data.storeys[0]
        .floors
        .regions[0]
        .id;
    c.set_floor_construction_for(
        &[floor0],
        FloorConstructionType::SuspendedFloor,
        Default::default(),
        Default::default(),
    );
    c.materialize(); // re-resolve mints a fresh region id

    let rec = c.current_record.as_ref().unwrap();
    let r = &rec.data.storeys[0].floors.regions[0];
    let fc = rec.data.floor_construction(r.construction_id).unwrap();
    assert_eq!(
        fc.construction_type,
        FloorConstructionType::SuspendedFloor,
        "the floor re-resolves to the assigned construction (centroid-keyed)"
    );
}

/// The "Edit attributes" group editor now works for FLOORS end to end: select
/// all floors, open the session (one default-construction group), set a type on
/// Set EXACTLY one active Structure mode (the modes are non-exclusive now, so a
/// test that wants a single kind has to add it then clear the others — adding
/// first keeps ≥1 active so each removal succeeds).
fn set_only_kind(c: &mut AppController, kind: SurfaceKind) {
    if !c.structure_kinds().contains(&kind) {
        c.toggle_structure_kind(kind);
    }
    for k in [
        SurfaceKind::Walls,
        SurfaceKind::Ceilings,
        SurfaceKind::Floors,
    ] {
        if k != kind && c.structure_kinds().contains(&k) {
            c.toggle_structure_kind(k);
        }
    }
}

/// the group, and the assigned floor construction lands on the region.
#[test]
fn structure_edit_sets_floor_group_construction() {
    use ber_model::ber::structural::{ConstructionType, FloorConstructionType};
    let mut c = dimensioned_square_controller();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Floors);
    c.set_structure_select_mode(Some(SelectMode::All));
    assert_eq!(
        c.structure_selected_ids().len(),
        1,
        "the one floor selected"
    );

    c.begin_structure_edit();
    let e = c.structure_edit().expect("a session opened for floors");
    assert_eq!(e.groups[0].ty.kind(), SurfaceKind::Floors);
    assert_eq!(e.groups.len(), 1, "one default-construction group");
    assert!(
        matches!(e.groups[0].ty, ConstructionType::Floor(_)),
        "the group's type is kind-tagged for floors"
    );

    c.set_structure_group_attrs(
        0,
        ConstructionType::Floor(FloorConstructionType::SuspendedFloor),
        Default::default(),
        Default::default(),
    );
    c.materialize();

    let rec = c.current_record.as_ref().unwrap();
    let r = &rec.data.storeys[0].floors.regions[0];
    let fc = rec.data.floor_construction(r.construction_id).unwrap();
    assert_eq!(fc.construction_type, FloorConstructionType::SuspendedFloor);
}

/// A Structure-checklist row's status by label.
fn structure_row(c: &AppController, label: &str) -> ChecklistStatus {
    c.structure_checklist()
        .into_iter()
        .find(|it| it.label == label)
        .unwrap_or_else(|| panic!("no structure row {label}"))
        .status
}

/// Attribute EVERY surface of `kind` with a real type + the given build-up, via
/// the Structure "Edit attributes" group editor.
fn attribute_all_surfaces(
    c: &mut AppController,
    kind: SurfaceKind,
    ty: ber_model::ber::structural::ConstructionType,
    makeup: ber_model::ber::structural::ConstructionMakeup,
) {
    set_only_kind(c, kind);
    c.set_structure_select_mode(Some(SelectMode::All));
    c.begin_structure_edit();
    let n = c.structure_edit().expect("session opened").groups.len();
    for g in 0..n {
        c.set_structure_group_attrs(g, ty, makeup.clone(), Default::default());
    }
    c.end_structure_edit();
    c.materialize();
}

/// The Structure checklist starts RED for every kind (the surfaces default to an
/// `Uninitialised` type / `Tbd` build-up — it used to be always green), and a
/// kind only turns green once EVERY surface of that kind is fully attributed.
/// This single-storey square fixture has walls + a floor but no roof region, so
/// "Ceilings" doubles as a negative case: an absent kind is NOT vacuously green.
#[test]
fn structure_checklist_tracks_per_surface_completion() {
    use ber_model::ber::structural::{
        ConstructionMakeup, ConstructionType, FloorConstructionType, WallConstructionType,
    };
    let mut c = dimensioned_square_controller();
    c.materialize();

    // Nothing attributed yet → every row red (previously always green).
    assert_eq!(structure_row(&c, "Walls"), ChecklistStatus::Incomplete);
    assert_eq!(structure_row(&c, "Floors"), ChecklistStatus::Incomplete);
    assert_eq!(structure_row(&c, "Ceilings"), ChecklistStatus::Incomplete);

    // Attribute the floor (Default build-up — no photo needed) → Floors green
    // while Walls stays red.
    attribute_all_surfaces(
        &mut c,
        SurfaceKind::Floors,
        ConstructionType::Floor(FloorConstructionType::SolidFloor),
        ConstructionMakeup::Default,
    );
    assert_eq!(structure_row(&c, "Floors"), ChecklistStatus::AllStoreys);
    assert_eq!(
        structure_row(&c, "Walls"),
        ChecklistStatus::Incomplete,
        "walls still unattributed"
    );

    // Attribute the walls too → Walls green.
    attribute_all_surfaces(
        &mut c,
        SurfaceKind::Walls,
        ConstructionType::Wall(WallConstructionType::CavityWall),
        ConstructionMakeup::Default,
    );
    assert_eq!(structure_row(&c, "Walls"), ChecklistStatus::AllStoreys);
    assert_eq!(structure_row(&c, "Floors"), ChecklistStatus::AllStoreys);

    // Ceilings has no surface in this fixture → it never goes vacuously green.
    assert_eq!(structure_row(&c, "Ceilings"), ChecklistStatus::Incomplete);
}

/// The Structure MODES are non-exclusive: several kinds can be active at once,
/// the `All` seed spans them, an "Edit attributes" session groups across them,
/// and the last active kind can't be toggled off (≥1 always stays).
#[test]
fn structure_modes_are_non_exclusive() {
    let mut c = dimensioned_square_controller();
    c.materialize();

    // Default: only Walls.
    assert_eq!(
        c.structure_kinds().iter().copied().collect::<Vec<_>>(),
        vec![SurfaceKind::Walls]
    );

    // Add Floors → both active; `All` selects walls AND the one floor.
    c.toggle_structure_kind(SurfaceKind::Floors);
    assert!(c.structure_kinds().contains(&SurfaceKind::Walls));
    assert!(c.structure_kinds().contains(&SurfaceKind::Floors));
    c.set_structure_select_mode(Some(SelectMode::All));
    assert!(
        c.structure_selected_ids().len() >= 2,
        "selection spans walls + floor: {}",
        c.structure_selected_ids().len()
    );

    // An edit session groups across both active kinds.
    c.begin_structure_edit();
    let kinds: std::collections::BTreeSet<_> = c
        .structure_edit()
        .expect("session")
        .groups
        .iter()
        .map(|g| g.ty.kind())
        .collect();
    assert!(
        kinds.contains(&SurfaceKind::Walls) && kinds.contains(&SurfaceKind::Floors),
        "groups span both active kinds: {kinds:?}"
    );
    c.end_structure_edit();

    // Toggle Walls off → {Floors}; toggling the last one off is a no-op.
    c.toggle_structure_kind(SurfaceKind::Walls);
    assert_eq!(
        c.structure_kinds().iter().copied().collect::<Vec<_>>(),
        vec![SurfaceKind::Floors]
    );
    c.toggle_structure_kind(SurfaceKind::Floors);
    assert_eq!(
        c.structure_kinds().iter().copied().collect::<Vec<_>>(),
        vec![SurfaceKind::Floors],
        "the last active mode stays active"
    );
}

/// "Specify…" build-up: opening the layer editor sets the group make-up to
/// `Layers`; a TYPED layer is saved to the app-wide library AND appended to the
/// build-up; picking via `set_structure_group_layers` replaces the layers.
#[test]
fn specify_layers_saves_to_library_and_builds_up() {
    use ber_model::ber::structural::{ConstructionLayer, ConstructionMakeup};
    let mut c = dimensioned_square_controller();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Walls);
    c.set_structure_select_mode(Some(SelectMode::All));
    c.begin_structure_edit();

    // Open the layer editor for group 0 → make-up becomes Layers, editor marked.
    c.begin_specify_layers(0);
    assert_eq!(c.structure_edit().unwrap().specifying, Some(0));
    assert!(matches!(
        c.structure_edit().unwrap().groups[0].makeup,
        ConstructionMakeup::Layers(_)
    ));

    // Set the build-up to one typed layer → saved to the library too.
    c.set_structure_group_layers(
        0,
        vec![ConstructionLayer::typed("EPS".into(), 100, 0.0, false)],
    );
    assert!(
        c.construction_layer_library()
            .iter()
            .any(|l| l.name() == "EPS" && l.thickness_mm == 100),
        "typed layer joined the reusable library"
    );
    let layers = match &c.structure_edit().unwrap().groups[0].makeup {
        ConstructionMakeup::Layers(v) => v.clone(),
        _ => panic!("expected Layers"),
    };
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].name(), "EPS");

    // Pick/append another (the library-pick path) → build-up has two, inside→out.
    let mut two = layers;
    two.push(ConstructionLayer::typed("Block".into(), 215, 1.33, false));
    c.set_structure_group_layers(0, two);
    assert!(matches!(
        &c.structure_edit().unwrap().groups[0].makeup,
        ConstructionMakeup::Layers(v) if v.len() == 2
    ));

    c.end_specify_layers();
    assert_eq!(c.structure_edit().unwrap().specifying, None);
}

/// A photo attached to a group is copied into the photo store and recorded on
/// the group's construction (the build-up floaty's camera button → controller).
#[test]
fn group_photo_copies_into_store_and_attaches() {
    let mut c = dimensioned_square_controller();
    c.materialize();
    let tmp = std::env::temp_dir().join(format!("ber_photos_{}", ItemId::default().0));
    c.set_photos_dir(tmp.clone());
    let src = std::env::temp_dir().join(format!("ber_src_{}.jpg", ItemId::default().0));
    std::fs::write(&src, b"jpeg-bytes").unwrap();

    set_only_kind(&mut c, SurfaceKind::Walls);
    c.set_structure_select_mode(Some(SelectMode::All));
    c.begin_structure_edit();
    c.add_group_photo(0, &src, PhotoQuality::Full);

    let g0 = &c.structure_edit().unwrap().groups[0];
    let rec = c.current_record.as_ref().unwrap();
    let cid = rec.data.storeys[0]
        .walls
        .regions
        .iter()
        .find(|r| g0.ids.contains(&r.id))
        .unwrap()
        .construction_id;
    let photos = &rec.data.wall_construction(cid).unwrap().base.photos;
    assert_eq!(photos.len(), 1, "photo recorded on the construction");
    let stored = std::path::Path::new(&photos[0].file_path);
    assert!(stored.exists(), "photo copied into the store");
    assert!(stored.starts_with(&tmp), "stored under the photos dir");

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&src);
}

/// The ceiling-insulation setter writes the spec onto the group's (shared)
/// ceiling construction — Unknown vs a thickness — mirrors it onto the open
/// session group, and the value survives a re-derive (the construction
/// registry persists across `materialize`).
#[test]
fn ceiling_insulation_setter_stores_mirrors_and_survives_rederive() {
    use ber_model::ber::structural::CeilingInsulation;
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.7).unwrap();
    c.materialize(); // derive the one flat roof region

    set_only_kind(&mut c, SurfaceKind::Ceilings);
    c.set_structure_select_mode(Some(SelectMode::All));
    assert_eq!(c.structure_selected_ids().len(), 1, "the one roof region");
    c.begin_structure_edit();
    assert_eq!(
        c.structure_edit().unwrap().groups[0].insulation,
        None,
        "seeded unspecified"
    );

    c.set_structure_group_insulation(0, Some(CeilingInsulation::ThicknessMm(150.0)));
    assert_eq!(
        c.structure_edit().unwrap().groups[0].insulation,
        Some(CeilingInsulation::ThicknessMm(150.0)),
        "mirrored onto the session group"
    );
    let ceiling_insulation = |c: &AppController| {
        let rec = c.current_record.as_ref().unwrap();
        let cid = rec.data.storeys[0].ceilings.regions[0].construction_id;
        rec.data.ceiling_construction(cid).unwrap().insulation
    };
    assert_eq!(
        ceiling_insulation(&c),
        Some(CeilingInsulation::ThicknessMm(150.0)),
        "stored on the construction"
    );

    c.set_structure_group_insulation(0, Some(CeilingInsulation::Unknown));
    assert_eq!(ceiling_insulation(&c), Some(CeilingInsulation::Unknown));

    c.materialize(); // the setter marked dirty — force the re-resolve
    assert_eq!(
        ceiling_insulation(&c),
        Some(CeilingInsulation::Unknown),
        "survives the re-resolve (the construction registry persists)"
    );
}

/// Completing "Edit attributes" is GATED on ceiling insulation: Done refuses
/// while the spec is missing, and while a specified thickness has no backing
/// photo; attaching the photo unblocks it. Reopening the editor re-seeds the
/// spec + photos from the construction. (Wall/floor sessions stay ungated —
/// `structure_checklist_tracks_per_surface_completion` closes those freely.)
#[test]
fn structure_edit_completion_gates_on_ceiling_insulation() {
    use ber_model::ber::structural::CeilingInsulation;
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.7).unwrap();
    c.materialize();
    let tmp = std::env::temp_dir().join(format!("ber_ins_photos_{}", ItemId::default().0));
    c.set_photos_dir(tmp.clone());
    let src = std::env::temp_dir().join(format!("ber_ins_src_{}.jpg", ItemId::default().0));
    std::fs::write(&src, b"jpeg-bytes").unwrap();

    set_only_kind(&mut c, SurfaceKind::Ceilings);
    c.set_structure_select_mode(Some(SelectMode::All));
    c.begin_structure_edit();
    assert_eq!(
        c.structure_edit().unwrap().blocker(),
        Some("enter the insulation thickness or mark it unknown")
    );
    c.end_structure_edit();
    assert!(
        c.structure_edit().is_some(),
        "Done refused while the insulation is unspecified"
    );

    c.set_structure_group_insulation(0, Some(CeilingInsulation::ThicknessMm(120.0)));
    assert_eq!(
        c.structure_edit().unwrap().blocker(),
        Some("a photo of the insulation is required")
    );
    c.end_structure_edit();
    assert!(
        c.structure_edit().is_some(),
        "a thickness without a photo still blocks"
    );

    c.add_group_photo(0, &src, PhotoQuality::High);
    assert_eq!(c.structure_edit().unwrap().blocker(), None);
    c.end_structure_edit();
    assert!(c.structure_edit().is_none(), "thickness + photo completes");

    // Reopen: the session re-seeds the spec + photo from the construction, so
    // the completed group stays completable.
    c.begin_structure_edit();
    let g = &c.structure_edit().unwrap().groups[0];
    assert_eq!(g.insulation, Some(CeilingInsulation::ThicknessMm(120.0)));
    assert_eq!(g.photos.len(), 1, "photo re-seeded");
    assert_eq!(c.structure_edit().unwrap().blocker(), None);

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&src);
}

/// The Ceilings checklist row requires the insulation spec on top of type +
/// build-up: a fully typed ceiling stays red until the insulation is
/// specified; declaring it UNKNOWN (which needs no photo) turns it green and
/// lets the session complete.
#[test]
fn ceilings_checklist_requires_insulation_spec() {
    use ber_model::ber::structural::{
        CeilingConstructionType, CeilingInsulation, ConstructionMakeup, ConstructionType,
    };
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.7).unwrap();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Ceilings);
    c.set_structure_select_mode(Some(SelectMode::All));
    c.begin_structure_edit();
    c.set_structure_group_attrs(
        0,
        ConstructionType::Ceiling(CeilingConstructionType::FlatRoof),
        ConstructionMakeup::Default,
        Default::default(),
    );
    c.materialize(); // re-resolve: the region now references the typed construction
    assert_eq!(
        structure_row(&c, "Ceilings"),
        ChecklistStatus::Incomplete,
        "type + build-up alone no longer complete a ceiling"
    );

    c.set_structure_group_insulation(0, Some(CeilingInsulation::Unknown));
    assert_eq!(
        structure_row(&c, "Ceilings"),
        ChecklistStatus::AllStoreys,
        "declared-unknown insulation completes without a photo"
    );
    c.end_structure_edit();
    assert!(
        c.structure_edit().is_none(),
        "unknown insulation lets the session complete"
    );
}

/// `ingest_photo` returns a provisional photo IMMEDIATELY (a fast byte-copy, no
/// decode) and finishes the heavy work on a background thread; once the job
/// lands, `apply_finished_photos` swaps the recompressed file + thumbnail into
/// the photo by id and deletes the provisional copy.
#[test]
fn ingest_photo_upgrades_provisional_in_background() {
    let mut c = dimensioned_square_controller();
    let tmp = std::env::temp_dir().join(format!("ber_ingest_{}", ItemId::default().0));
    c.set_photos_dir(tmp.clone());

    let src = tmp.join("src.png");
    std::fs::create_dir_all(&tmp).unwrap();
    image::RgbImage::from_pixel(800, 600, image::Rgb([10, 20, 30]))
        .save(&src)
        .unwrap();

    // Synchronous: a provisional photo pointing at a fast copy (no decode yet).
    let photo = c
        .ingest_photo(&src, PhotoQuality::Low)
        .expect("provisional photo");
    let id = photo.id;
    assert!(
        photo.file_path.contains("_orig"),
        "provisional points at the fast copy: {}",
        photo.file_path
    );
    assert!(photo.thumbnail_path.is_none(), "no thumbnail yet");
    c.current_record.as_mut().unwrap().data.photos.push(photo);

    // Wait for the background job, then apply it.
    let mut spins = 0;
    while c.photos_processing() && spins < 2000 {
        std::thread::sleep(std::time::Duration::from_millis(2));
        spins += 1;
    }
    assert!(spins < 2000, "background job finished");
    assert!(c.apply_finished_photos(), "a photo was upgraded");

    let stored = &c.current_record.as_ref().unwrap().data.photos[0];
    assert_eq!(stored.id, id);
    assert!(
        !stored.file_path.contains("_orig"),
        "upgraded off the provisional copy: {}",
        stored.file_path
    );
    assert!(
        stored
            .thumbnail_path
            .as_ref()
            .is_some_and(|t| std::path::Path::new(t).exists()),
        "thumbnail generated"
    );
    assert!(
        std::path::Path::new(&stored.file_path).exists(),
        "final file exists"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The compact-tier recompression unit (the Android path — desktop now stores
/// originals verbatim, so this tests `recompress` directly to stay
/// platform-independent): a large source downscales to within the tier's pixel
/// cap, re-encodes to JPEG at the tier quality, and lands smaller than the
/// original.
#[test]
fn recompress_downscales_and_shrinks() {
    use image::GenericImageView;

    let tmp = std::env::temp_dir().join(format!("ber_recompress_{}", ItemId::default().0));
    std::fs::create_dir_all(&tmp).unwrap();

    // A 3000×2000 noisy image: big enough that downscaling to the Low cap
    // (1280px) and re-encoding is a clear, non-trivial reduction.
    let src = tmp.join("big.png");
    let mut img = image::RgbImage::new(3000, 2000);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
    }
    img.save(&src).unwrap();
    let src_len = std::fs::metadata(&src).unwrap().len();

    // Low tier: 1280 px cap, quality 50.
    let dest = tmp.join("out.jpg");
    assert!(
        AppController::recompress(&src, &dest, 1280, 50),
        "recompress succeeds"
    );
    assert!(dest.exists(), "recompressed file written");

    let (w, h) = image::ImageReader::open(&dest)
        .unwrap()
        .with_guessed_format()
        .unwrap()
        .decode()
        .unwrap()
        .dimensions();
    assert!(
        w.max(h) <= 1280,
        "downscaled to the Low tier pixel cap, got {w}x{h}"
    );
    let stored_len = std::fs::metadata(&dest).unwrap().len();
    assert!(
        stored_len < src_len,
        "recompressed ({stored_len}) smaller than original ({src_len})"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The group PHASE is editable — defaulted to the derived value but overridable
/// in the editor, and the override survives the re-resolve (stored in the
/// durable side-table alongside type + make-up).
#[test]
fn group_phase_is_overridable() {
    use ber_model::ber::common::ConstructionPhase;
    use ber_model::ber::structural::{ConstructionType, FloorConstructionType};
    let mut c = dimensioned_square_controller();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Floors);
    c.set_structure_select_mode(Some(SelectMode::All));
    c.begin_structure_edit();

    // Override the floor group's phase to Extension1 (the storey has no drawn
    // zones, so the derived phase is Original).
    c.set_structure_group_attrs(
        0,
        ConstructionType::Floor(FloorConstructionType::SolidFloor),
        Default::default(),
        ConstructionPhase::Extension1,
    );
    c.materialize();

    let rec = c.current_record.as_ref().unwrap();
    let r = &rec.data.storeys[0].floors.regions[0];
    let fc = rec.data.floor_construction(r.construction_id).unwrap();
    assert_eq!(
        fc.base.construction_phase,
        ConstructionPhase::Extension1,
        "the chosen phase overrides the derived one and survives re-resolve"
    );
}

/// A renamed group's label PERSISTS — it's stored on the construction (the
/// former `description` field, now `name`), so reopening "Edit attributes" after
/// a re-resolve shows the chosen name instead of the "Group N" default.
#[test]
fn group_name_persists_across_reopen() {
    let mut c = dimensioned_square_controller();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Floors);
    c.set_structure_select_mode(Some(SelectMode::All));
    c.begin_structure_edit();
    assert_eq!(c.structure_edit().expect("session").groups.len(), 1);
    assert_eq!(c.structure_edit().unwrap().groups[0].name, "Group 1");

    c.set_structure_group_name(0, "Kitchen".to_string());
    c.end_structure_edit();
    c.materialize(); // re-resolve: the name on the construction must survive it

    c.begin_structure_edit();
    assert_eq!(
        c.structure_edit().expect("reopened").groups[0].name,
        "Kitchen",
        "the stored construction name is read back as the group label"
    );
}

/// A carve splits the AREA kind it's drawn over even when a higher-priority mode
/// (walls) is also active. Regression: the old code split only the single
/// "primary" kind (wall → roof → floor), so an interior carve in floor+wall mode
/// tried to split the WALL, found no crossed wall, and abandoned — leaving the
/// floor unsplit and the whole region merely selected. Now every active kind's
/// split is applied, so the floor (and roof, by the same path) splits.
#[test]
fn carve_splits_the_area_kind_even_with_walls_also_active() {
    let mut c = dimensioned_square_controller();
    c.materialize();
    assert_eq!(c.current_surface_counts().floors, 1, "one floor to start");

    // Floors + Walls both active (walls outranks floors in the priority order).
    set_only_kind(&mut c, SurfaceKind::Floors);
    c.toggle_structure_kind(SurfaceKind::Walls);
    assert_eq!(c.structure_kinds().len(), 2);

    c.set_structure_select_mode(Some(SelectMode::Carve));
    // Interior 4×4 region — crosses no wall, so only the floor should split.
    c.apply_structure_carve_stroke(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)]);
    complete_interior_length_loop(&mut c);
    skip_position_prompts(&mut c);
    // The carve is staged (deferred); the split happens when Edit attributes opens.
    assert_eq!(
        c.current_surface_counts().floors,
        1,
        "split deferred until edit"
    );
    c.begin_structure_edit();

    assert_eq!(
        c.current_surface_counts().floors,
        2,
        "the floor split even though wall mode was also active"
    );
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .floor_splits
            .len(),
        1,
        "the floor split is stored durably"
    );
}

/// The Structure selection + an open "Edit attributes" session SURVIVE a
/// re-resolve. Walls (and floors) re-mint region ids on every resolve, so
/// without re-pointing by durable key the selected ids would dangle: the canvas
/// shading blanks out and a multi-group edit silently drops every apply after
/// the first (the apply itself bumps the revision and triggers the re-resolve).
/// Regression for that bug — the selection must follow the geometry, like the
/// riser/elevation selection.
#[test]
fn structure_selection_survives_re_resolve() {
    let mut c = dimensioned_square_controller();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Walls);
    c.set_structure_select_mode(Some(SelectMode::All));
    let n = c.structure_selected_ids().len();
    assert!(n >= 1, "at least one wall selected");
    c.begin_structure_edit();
    assert!(c.structure_edit().is_some(), "edit session opened");

    // Force a re-resolve — re-mints every wall region id.
    c.commit_to_ber().unwrap();

    let rec = c.current_record.as_ref().unwrap();
    let live: std::collections::HashSet<_> = rec.data.storeys[0]
        .walls
        .regions
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        c.structure_selected_ids().len(),
        n,
        "selection count preserved across the re-resolve"
    );
    assert!(
        c.structure_selected_ids()
            .iter()
            .all(|id| live.contains(id)),
        "every selected id re-pointed onto a LIVE wall region (not a stale id)"
    );
    let edit = c.structure_edit().expect("session still open");
    assert!(
        edit.groups
            .iter()
            .flat_map(|g| &g.ids)
            .all(|id| live.contains(id)),
        "the edit session's group ids re-pointed onto live regions too"
    );
}

/// A wall's resolved construction phase, via its region's `construction_id`.
fn wall_phase(c: &AppController, region_idx: usize) -> ConstructionPhase {
    let rec = c.current_record.as_ref().unwrap();
    let cid = rec.data.storeys[0].walls.regions[region_idx].construction_id;
    rec.data
        .wall_construction(cid)
        .unwrap()
        .base
        .construction_phase
        .clone()
}

/// Construction phases entered up front are stored on the BER and surface as
/// labelled tagging choices.
#[test]
fn set_construction_phases_populates_choices() {
    use ber_model::ber::common::ConstructionPhases;
    let mut c = dimensioned_square_controller();
    let mut phases = ConstructionPhases::default();
    phases.set_year(ConstructionPhase::Original, 1985);
    phases.set_year(ConstructionPhase::Extension1, 2008);
    c.set_construction_phases(phases);

    let choices = c.construction_phase_choices();
    assert_eq!(choices.len(), 2, "two phases offered");
    assert_eq!(choices[0].0, ConstructionPhase::Original);
    assert!(choices[0].1.contains("1985"), "label carries the year");
    assert_eq!(choices[1].0, ConstructionPhase::Extension1);
    assert!(choices[1].1.contains("2008"));
}

/// A phase polygon stamps its phase onto walls whose midpoint falls inside it;
/// walls outside stay `Unassigned` (the untagged-remainder default).
#[test]
fn phase_polygon_stamps_contained_walls() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    c.set_all_remaining_exposed(); // all 4 perimeter walls → regions
                                   // Tag the right side (x >= 6) as Extension1.
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(6., -1.), p(12., -1.), p(12., 12.), p(6., 12.)],
        phase: ConstructionPhase::Extension1,
    });
    c.commit_to_ber().unwrap();

    let rec = c.current_record.as_ref().unwrap();
    let walls = rec.data.storeys[0].walls.regions.clone();
    assert!(!walls.is_empty(), "walls became regions");
    for (i, w) in walls.iter().enumerate() {
        let mid = w.base_midpoint().unwrap();
        let phase = wall_phase(&c, i);
        if mid.x >= 6.0 {
            assert_eq!(
                phase,
                ConstructionPhase::Extension1,
                "wall {i} (right) tagged"
            );
        } else {
            // The untagged remainder is now `Unassigned` (the gate's
            // "left unassigned" state), not the implicit `Original` it used to be.
            assert_eq!(
                phase,
                ConstructionPhase::Unassigned,
                "wall {i} (other) unassigned"
            );
        }
    }
}

/// A floor phase area splits the storey floor into multiple regions, each
/// carrying its own phase (tagged zone + remainder).
#[test]
fn phase_area_splits_floor_into_regions() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
        phase: ConstructionPhase::Extension1,
    });
    c.commit_to_ber().unwrap();
    let rec = c.current_record.as_ref().unwrap();
    let floors = &rec.data.storeys[0].floors.regions;
    assert_eq!(floors.len(), 2, "floor split into tagged zone + remainder");
    let phases: Vec<ConstructionPhase> = floors
        .iter()
        .map(|f| {
            rec.data
                .floor_construction(f.construction_id)
                .unwrap()
                .base
                .construction_phase
                .clone()
        })
        .collect();
    assert!(phases.contains(&ConstructionPhase::Extension1));
    // The remainder (the untagged half) is now `Unassigned`, not `Original`.
    assert!(phases.contains(&ConstructionPhase::Unassigned));
}

/// The floor split survives a DB round-trip: reopening rebuilds the split
/// regions, reconstructs the session phase areas, and the editor outline is
/// re-merged from the zones (one clean footprint, not a single zone).
#[test]
fn phase_split_survives_reload() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
        phase: ConstructionPhase::Extension1,
    });
    c.commit_to_ber().unwrap();
    let id = c.current_record.as_ref().unwrap().id;

    c.open_ber(&id).unwrap();
    let floors = &c.current_record.as_ref().unwrap().data.storeys[0]
        .floors
        .regions;
    assert_eq!(floors.len(), 2, "split persisted + reloaded");
    assert!(
        c.phase_regions.get(&0).is_some_and(|v| !v.is_empty()),
        "phase areas reconstructed from floor regions"
    );
    // Outline merged back to the full 10x10 footprint (~100 m²), cleaned to
    // 4 corners (no phantom vertex at the dissolved seam) — not a single zone.
    let pts = c.document.outline.floor_plan.get_points();
    assert_eq!(pts.len(), 4, "merged outline is a clean rectangle");
    let n = pts.len();
    let mut a2 = 0.0f32;
    for i in 0..n {
        let (p, q) = (pts[i], pts[(i + 1) % n]);
        a2 += p.x * q.y - q.x * p.y;
    }
    let area = (a2.abs() / 2.0) as f64;
    assert!(
        (area - 100.0).abs() < 1.0,
        "merged outline ~100 m², got {area}"
    );
}

/// A floor-step area must be DIMENSIONED before its level is assigned (the
/// exact boundary decides which floor pieces step). So: carve → dimension the
/// drawn walls → only THEN the amount field → assign stores the dimensioned
/// ring and returns to the outline.
#[test]
fn step_area_is_dimensioned_before_its_level_is_assigned() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    // 2x2 dimensioned storey.
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(2.0, 0.0),
        Point2f32::new(2.0, 2.0),
        Point2f32::new(0.0, 2.0),
        Point2f32::new(0.05, 0.05),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.1, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.sync_active(); // flush into the outline slot so the step carves against it

    // Draw an interior step area (a ~0.9m square that doesn't reach a wall).
    c.set_step_mode(true);
    c.apply_step_region_stroke_event(&[
        (0.5, 0.5),
        (1.4, 0.5),
        (1.4, 1.4),
        (0.5, 1.4),
        (0.55, 0.55),
    ]);

    // It must prompt for dimensions — NOT be immediately assignable.
    assert!(
        c.dimensioning_active(),
        "step area prompts to be dimensioned"
    );
    assert!(!c.step_pick_pending(), "no level entry until dimensioned");

    // Dimension every drawn wall.
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        c.apply_dimension(0.9).expect("dimension applies");
        guard += 1;
    }
    // Lengths done — but an interior area's POSITION is still approximate,
    // so the position prompts gate the amount field until entered or skipped.
    assert!(!c.step_pick_pending(), "no level entry until positioned");
    skip_position_prompts(&mut c);
    assert!(c.step_pick_pending(), "now the amount field shows");
    assert!(
        c.pending_step_polygon().len() >= 4,
        "dimensioned ring available: {:?}",
        c.pending_step_polygon()
    );

    // Assign the level → stored region, back to the outline.
    c.assign_step_level_to_pending(-0.3).expect("level applies");
    assert!(!c.step_pick_pending(), "level consumed");
    let regions = c.step_region_polygons();
    assert_eq!(
        regions.len(),
        1,
        "the dimensioned step area is stored: {regions:?}"
    );
    assert_eq!(regions[0].1, "-0.30 m", "label carries the signed level");
    // And the outline is the active buffer again, intact (the 2x2 square).
    assert_eq!(
        c.document.floor_plan.get_points().len(),
        4,
        "outline restored as the live buffer: {:?}",
        c.document.floor_plan.get_points()
    );
}

/// A near-zero step amount (|level| < 0.01 m) is rejected: the pending area
/// stays open (the user retypes or cancels) and nothing is stored.
#[test]
fn step_area_zero_amount_is_rejected_and_area_stays_pending() {
    let mut c = dimensioned_square_controller();
    c.set_step_mode(true);
    c.apply_step_region_stroke_event(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)]);
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        c.apply_dimension(4.0).expect("dimension applies");
        guard += 1;
    }
    skip_position_prompts(&mut c); // accept the drawn position
    assert!(c.step_pick_pending(), "carved area awaits its amount");

    let err = c
        .assign_step_level_to_pending(0.005)
        .expect_err("a near-zero step is rejected");
    assert!(
        err.contains("0.01"),
        "the rejection names the minimum so the UI can show it: {err}"
    );
    assert!(
        c.step_pick_pending(),
        "a zero step is no step — the prompt stays open"
    );
    assert!(
        c.pending_step_polygon().len() >= 4,
        "the pending area remains"
    );
    assert!(c.step_region_polygons().is_empty(), "nothing was stored");
}

/// A floor-step area splits the storey floor into level regions: the stepped
/// zone carries the assigned `level_m`, the remainder stays at the datum.
/// (Counts aren't asserted — the split may produce several pieces.)
#[test]
fn step_area_splits_floor_into_level_regions() {
    let mut c = dimensioned_square_controller();
    // Carve an interior 4x4 step area, dimension it, lower it by 0.3 m.
    c.set_step_mode(true);
    c.apply_step_region_stroke_event(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)]);
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        c.apply_dimension(4.0).expect("dimension applies");
        guard += 1;
    }
    skip_position_prompts(&mut c); // accept the drawn position
    assert!(c.step_pick_pending(), "carved area awaits its amount");
    c.assign_step_level_to_pending(-0.3).expect("level applies");
    c.materialize();

    let rec = c.current_record.as_ref().unwrap();
    let floors = &rec.data.storeys[0].floors.regions;
    assert!(
        floors.iter().any(|f| (f.level_m + 0.3).abs() < 1e-6),
        "a floor region carries the lowered level: {floors:?}"
    );
    assert!(
        floors.iter().any(|f| f.level_m == 0.0),
        "the remainder stays at the storey datum: {floors:?}"
    );
}

/// The level split survives a DB round-trip: reopening rebuilds the session
/// step areas from the levelled floor regions, so the NEXT materialize keeps
/// the split instead of flattening the floor.
#[test]
fn step_split_survives_reload() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    c.step_regions.entry(0).or_default().push(StepRegion {
        polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
        level_m: -0.3,
    });
    c.commit_to_ber().unwrap();
    let id = c.current_record.as_ref().unwrap().id;

    c.open_ber(&id).unwrap();
    let floors = &c.current_record.as_ref().unwrap().data.storeys[0]
        .floors
        .regions;
    assert!(
        floors.iter().any(|f| (f.level_m + 0.3).abs() < 1e-6),
        "split persisted + reloaded: {floors:?}"
    );
    assert!(
        c.step_regions.get(&0).is_some_and(|v| !v.is_empty()),
        "step areas reconstructed from the levelled floor regions"
    );

    // A re-derive WITHOUT further edits must keep the split (the reconstructed
    // session areas re-apply it; without them the floor would flatten).
    c.commit_to_ber().unwrap();
    let floors = &c.current_record.as_ref().unwrap().data.storeys[0]
        .floors
        .regions;
    assert!(
        floors.iter().any(|f| (f.level_m + 0.3).abs() < 1e-6),
        "the next materialize keeps the level split: {floors:?}"
    );
}

/// REGRESSION: an UNTAGGED storey's outline must survive commit→reload
/// unchanged — no `geo` re-normalisation. When the partition + merged-outline
/// ran unconditionally (even with no phase zones), the rebuilt ring was
/// distorted, which inflated the plan extent and made the collinearity review
/// flag far-apart (opposite-side) walls. An L-shape exercises the concave
/// case geo is most likely to mangle.
#[test]
fn untagged_outline_survives_reload_without_distortion() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(
        &[
            p(0., 0.),
            p(10., 0.),
            p(10., 6.),
            p(4., 6.),
            p(4., 10.),
            p(0., 10.),
            p(0.05, 0.05),
        ],
        0.1,
        true,
    )
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
    let before = c.document.outline.floor_plan.get_points();
    // Untagged → exactly one floor region (no split).
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .floors
            .regions
            .len(),
        1,
        "untagged storey stays a single floor region"
    );

    let id = c.current_record.as_ref().unwrap().id;
    c.open_ber(&id).unwrap();
    let after = c.document.outline.floor_plan.get_points();

    assert_eq!(
        before.len(),
        after.len(),
        "vertex count preserved across reload (no geo-introduced/dropped corners): \
         before {before:?} after {after:?}"
    );
    for b in &before {
        assert!(
            after
                .iter()
                .any(|a| (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3),
            "vertex {b:?} missing/moved after reload; got {after:?}"
        );
    }
}

/// A wall takes the construction phase of the partitioned FLOOR zone on its
/// interior side: the right perimeter wall, whose interior lies in the tagged
/// extension zone, becomes Extension1; the left perimeter wall stays Unassigned
/// (the untagged remainder).
/// (Walls track the floor partition — no separate wall-only phase heuristic.)
#[test]
fn wall_takes_the_phase_of_its_interior_floor_zone() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    c.set_all_remaining_exposed();
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(6., -1.), p(12., -1.), p(12., 12.), p(6., 12.)],
        phase: ConstructionPhase::Extension1,
    });
    c.commit_to_ber().unwrap();
    let walls = c.current_record.as_ref().unwrap().data.storeys[0]
        .walls
        .regions
        .clone();
    let right_idx = walls
        .iter()
        .position(|w| w.base_midpoint().unwrap().x >= 6.0)
        .unwrap();
    let left_idx = walls
        .iter()
        .position(|w| w.base_midpoint().unwrap().x < 6.0)
        .unwrap();
    assert_eq!(wall_phase(&c, right_idx), ConstructionPhase::Extension1);
    // The left wall's interior floor zone is the untagged remainder → Unassigned.
    assert_eq!(wall_phase(&c, left_idx), ConstructionPhase::Unassigned);
}

/// A tagged phase survives a reload: the durable record is the floor regions'
/// `construction_id`, and the genuine reload path REBUILDS the session phase
/// regions from those floors (`reconstruct_phase_regions_for`) before the next
/// commit re-partitions. Simulate that here — clear the session regions, then
/// reconstruct — and the wall keeps its phase. (Clearing WITHOUT reconstructing
/// is not the reload path and is not expected to preserve anything.)
#[test]
fn phase_survives_reload_via_reconstruction() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    c.set_all_remaining_exposed();
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(6., -1.), p(12., -1.), p(12., 12.), p(6., 12.)],
        phase: ConstructionPhase::Extension1,
    });
    c.commit_to_ber().unwrap();
    let right_idx = c.current_record.as_ref().unwrap().data.storeys[0]
        .walls
        .regions
        .iter()
        .position(|w| w.base_midpoint().unwrap().x >= 6.0)
        .unwrap();
    assert_eq!(wall_phase(&c, right_idx), ConstructionPhase::Extension1);

    // Simulate reload: drop the session regions, then rebuild them from the
    // persisted floor tags exactly as `open_ber` does, and re-commit.
    c.phase_regions.clear();
    c.reconstruct_phase_regions_for(0);
    c.commit_to_ber().unwrap();
    assert_eq!(
        wall_phase(&c, right_idx),
        ConstructionPhase::Extension1,
        "the tag is rebuilt from the floors on reload"
    );
}

/// Add a finalised living area to the active storey's editing model, so
/// `has_living_area` / the "Living area" checklist row go green.
fn add_finalised_living_area(c: &mut AppController) {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let plan = FloorPlan::from_clean_ring(&[p(1., 1.), p(3., 1.), p(3., 3.), p(1., 3.)]).unwrap();
    let idx = c.current_storey_idx.unwrap_or(0);
    c.document.storeys[idx].living_areas.push(ShapeSlot {
        dimensioning: DimensionState {
            known_params: plan.all_active_params(),
            scale_set: true,
            ..Default::default()
        },
        floor_plan: plan,
    });
}

/// Status of one checklist row by label.
fn checklist_status_of(c: &AppController, label: &str) -> ChecklistStatus {
    c.stage_a_checklist()
        .into_iter()
        .find(|it| it.label == label)
        .unwrap_or_else(|| panic!("no checklist row {label}"))
        .status
}

/// The "Roof" checklist row gates Mapping: red until at least one roof area is
/// drawn, green after — so `mapping_complete` requires a roof.
#[test]
fn roof_row_gates_mapping_complete() {
    let mut c = dimensioned_square_controller();
    assert_eq!(
        checklist_status_of(&c, "Roof"),
        ChecklistStatus::Incomplete,
        "no roof yet"
    );
    // Draw a (flat) roof over the storey's ceiling.
    c.set_default_ceiling_height(2.4).unwrap();
    c.set_roof_mode(true);
    c.confirm_all_storeys_added_roof();
    c.roof_region_tap(5.0, 5.0, 0.5);
    c.pick_roof_area_shape(RoofShape::Flat);
    assert_eq!(
        checklist_status_of(&c, "Roof"),
        ChecklistStatus::AllStoreys,
        "a roof area satisfies the row"
    );
}

/// The three new Mapping checklist rows exist and each gates `mapping_complete`:
/// a dwelling with no living area / an Unassigned floor / Unset surfaces is NOT
/// mapping-complete; once all three are satisfied (plus the pre-existing rows)
/// it is.
#[test]
fn new_checklist_rows_gate_mapping_complete() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    // The pre-existing rows (Measurements/Heights/Exposures) need satisfying too,
    // so this dwelling starts incomplete regardless — assert the three NEW rows
    // are present and each starts red.
    let labels: Vec<&'static str> = c.stage_a_checklist().iter().map(|it| it.label).collect();
    assert!(
        labels.contains(&"Floor exposures"),
        "Floor exposures row present"
    );
    assert!(
        labels.contains(&"Ceiling exposures"),
        "Ceiling exposures row present"
    );
    assert!(
        labels.contains(&"Construction phase"),
        "Construction phase row present"
    );
    assert!(labels.contains(&"Living area"), "Living area row present");
    assert_eq!(
        checklist_status_of(&c, "Living area"),
        ChecklistStatus::Incomplete,
        "no living area yet"
    );
    assert!(!c.mapping_complete(), "incomplete dwelling is not complete");

    // Living area → that row goes green.
    add_finalised_living_area(&mut c);
    assert_eq!(
        checklist_status_of(&c, "Living area"),
        ChecklistStatus::AllStoreys,
        "a finalised living area satisfies the row"
    );

    // Construction phase: tag the whole storey Original (the one-tap), settle the
    // deferred derive, and the row goes green.
    c.assign_rest_to_original();
    c.commit_to_ber().unwrap();
    assert_eq!(
        checklist_status_of(&c, "Construction phase"),
        ChecklistStatus::AllStoreys,
        "rest-to-Original satisfies the phase row"
    );

    // Surfaces needs ceiling (roof) regions to exist — set a default ceiling
    // height so the storey resolves a roof. Neither the lowest floor nor the top
    // ceiling is auto-classified any more (both are left for the assessor), so
    // label BOTH manually, then the stamp pass lands them on the resolved regions
    // in the SAME commit. ONE commit must settle it — this single-commit
    // assertion is the regression guard for the lag bug.
    c.set_default_ceiling_height(2.4).unwrap();
    // Materialize so the roof region exists to read its centroid (the floor
    // already exists; the roof only resolves once a ceiling height is set).
    c.materialize();
    let fkey = document::centroid_key(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .floors
            .regions[0]
            .centroid(),
    );
    c.document.storeys[0]
        .floor_exposure
        .insert(fkey, ber_model::ber::structural::FloorExposure::Ground);
    let ckey = document::centroid_key(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .ceilings
            .regions[0]
            .centroid(),
    );
    c.document.storeys[0].ceiling_exposure.insert(
        ckey,
        ber_model::ber::structural::CeilingExposure::ExposedRoof,
    );
    c.commit_to_ber().unwrap();
    assert_eq!(
        checklist_status_of(&c, "Floor exposures"),
        ChecklistStatus::AllStoreys,
        "the labelled floor satisfies the floor-exposures row in one commit"
    );
    assert_eq!(
        checklist_status_of(&c, "Ceiling exposures"),
        ChecklistStatus::AllStoreys,
        "the labelled ceiling satisfies the ceiling-exposures row in one commit"
    );

    // Wall exposures is the last pre-existing gate — paint the perimeter.
    c.set_all_remaining_exposed();
    // Drive the pending phase area (if any from rest-to-Original) is already
    // committed; nothing left in flight. Re-assert the new rows are green.
    c.commit_to_ber().unwrap();
    let _ = p; // p used above via add_finalised_living_area's closure shape only
    for row in [
        "Floor exposures",
        "Ceiling exposures",
        "Construction phase",
        "Living area",
    ] {
        assert_eq!(
            checklist_status_of(&c, row),
            ChecklistStatus::AllStoreys,
            "{row} stays green"
        );
    }
}

/// `assign_rest_to_original` stamps the untagged remainder as `Original`, so
/// `document::construction_phase_complete` passes for the storey.
#[test]
fn assign_rest_to_original_satisfies_construction_phase_gate() {
    let mut c = dimensioned_square_controller();
    // Before: the whole floor is the untagged remainder → Unassigned → gate fails.
    c.commit_to_ber().unwrap();
    assert!(
        !document::construction_phase_complete(&c.current_record.as_ref().unwrap().data, 0),
        "an Unassigned floor fails the construction-phase gate"
    );

    c.assign_rest_to_original();
    c.commit_to_ber().unwrap();
    let ber = &c.current_record.as_ref().unwrap().data;
    assert!(
        ber.storeys[0].floors.regions.iter().all(|fr| ber
            .floor_construction(fr.construction_id)
            .unwrap()
            .base
            .construction_phase
            == ConstructionPhase::Original),
        "every floor region is now Original"
    );
    assert!(
        document::construction_phase_complete(ber, 0),
        "rest-to-Original satisfies the gate"
    );
}

/// After splitting a wall, tapping a leftover piece selects just that piece
/// (the coverage range under the tap), not the whole wall.
#[test]
fn tapping_a_split_wall_selects_the_piece_under_the_tap() {
    let mut c = dimensioned_square_controller();
    let (_, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();
    let along = |d: f32| {
        let t = d / len;
        (a0.x + t * (b0.x - a0.x), a0.y + t * (b0.y - a0.y))
    };

    c.set_exposure_mode(true);
    // Select the whole wall, mark the middle [0.4L, 0.6L] semi-exposed.
    let mid = along(len * 0.5);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_exposure_section_range(len * 0.4, len * 0.6);
    c.set_wall_exposure(WallExposure::SemiExposed);

    // Tap the LEFT leftover piece → selects (0, 0.4L), not the whole wall.
    let left = along(len * 0.1);
    c.exposure_pointer_down(left.0 as f64, left.1 as f64, 0.5);
    let sel = c.exposure_selection().expect("a piece is selected");
    assert!(
        sel.start_m.abs() < 0.05,
        "left piece starts at 0: {}",
        sel.start_m
    );
    assert!(
        (sel.end_m - len * 0.4).abs() < 0.05,
        "left piece ends at the section start: {}",
        sel.end_m
    );

    // Tap the semi piece → selects (0.4L, 0.6L).
    let centre = along(len * 0.5);
    c.exposure_pointer_down(centre.0 as f64, centre.1 as f64, 0.5);
    let sel2 = c.exposure_selection().unwrap();
    assert!((sel2.start_m - len * 0.4).abs() < 0.05);
    assert!((sel2.end_m - len * 0.6).abs() < 0.05);
}

/// Dragging a selection handle moves that end of the section selection.
#[test]
fn drag_updates_the_section_selection() {
    let mut c = dimensioned_square_controller();
    let (_, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
    let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();

    c.set_exposure_mode(true);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5); // select, sel = (0, len)
    let sel0 = c.exposure_selection().expect("a wall is selected");
    assert!(
        (sel0.end_m - len).abs() < 0.01,
        "end starts at the far corner"
    );
    // Grab the END handle (at the far corner) and drag it to the midpoint.
    c.exposure_pointer_down(b0.x as f64, b0.y as f64, 0.5);
    c.exposure_pointer_drag(mid.0 as f64, mid.1 as f64, 0.2);
    let sel1 = c.exposure_selection().unwrap();
    assert!(
        (sel1.end_m - len * 0.5).abs() < 0.1,
        "dragged end to mid: {}",
        sel1.end_m
    );
    c.exposure_pointer_up();
}

/// A dragged section handle SNAPS to a construction-phase region edge
/// crossing the wall when within tolerance — and doesn't when dragged well
/// away from any candidate.
#[test]
fn exposure_drag_snaps_to_a_phase_line() {
    let mut c = dimensioned_square_controller();
    let (_, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();
    let along = |d: f32| {
        let t = d / len;
        (a0.x + t * (b0.x - a0.x), a0.y + t * (b0.y - a0.y))
    };
    // A phase region attached to wall0's first 4 m: its far edge meets the
    // wall at 4.0 m along (built perpendicular so it works whatever wall0's
    // orientation).
    // (Which normal side the quad sits on doesn't matter — the crossing math
    // only needs its far edge to TOUCH the wall at 4.0.)
    let (nx, ny) = (-(b0.y - a0.y) / len, (b0.x - a0.x) / len);
    let p4 = along(4.0);
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![
            a0,
            Point2f32::new(p4.0, p4.1),
            Point2f32::new(p4.0 + nx * 3.0, p4.1 + ny * 3.0),
            Point2f32::new(a0.x + nx * 3.0, a0.y + ny * 3.0),
        ],
        phase: ConstructionPhase::Original,
    });

    c.set_exposure_mode(true);
    let mid = along(len * 0.5);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5); // whole wall
    c.exposure_pointer_down(b0.x as f64, b0.y as f64, 0.5); // grab END handle
                                                            // Within tolerance of the phase edge at 4.0 → lands ON it.
    let near = along(3.93);
    c.exposure_pointer_drag(near.0 as f64, near.1 as f64, 0.2);
    let sel = c.exposure_selection().unwrap();
    assert!(
        (sel.end_m - 4.0).abs() < 1e-3,
        "snapped to the phase line at 4.0: {}",
        sel.end_m
    );
    assert_eq!(sel.snap_hint, Some("phase line"));
    // Well away from every candidate (0, 4, 10) → no snap, hint gone.
    let far = along(3.0);
    c.exposure_pointer_drag(far.0 as f64, far.1 as f64, 0.2);
    let sel2 = c.exposure_selection().unwrap();
    assert!(
        (sel2.end_m - 3.0).abs() < 0.01,
        "no snap when far: {}",
        sel2.end_m
    );
    assert_eq!(sel2.snap_hint, None);
    c.exposure_pointer_up();
}

/// A dragged handle snaps to an EXISTING exposure-section boundary on the
/// same wall, and to the wall's own endpoint.
#[test]
fn exposure_drag_snaps_to_section_edges_and_wall_ends() {
    let mut c = dimensioned_square_controller();
    let (_, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();
    let along = |d: f32| {
        let t = d / len;
        (a0.x + t * (b0.x - a0.x), a0.y + t * (b0.y - a0.y))
    };
    c.set_exposure_mode(true);
    // Paint [2, 5] semi-exposed → its edges become snap candidates.
    let mid = along(len * 0.5);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_exposure_section_range(2.0, 5.0);
    c.set_wall_exposure(WallExposure::SemiExposed);

    // Select the trailing gap (5, len), grab its START handle (at 5.0) and
    // drag it slightly off — it snaps back onto the section edge.
    let gap = along(7.5);
    c.exposure_pointer_down(gap.0 as f64, gap.1 as f64, 0.5);
    let start = along(5.0);
    c.exposure_pointer_down(start.0 as f64, start.1 as f64, 0.2);
    let off = along(5.12);
    c.exposure_pointer_drag(off.0 as f64, off.1 as f64, 0.2);
    let sel = c.exposure_selection().unwrap();
    assert!(
        (sel.start_m - 5.0).abs() < 1e-3,
        "snapped to the section edge at 5.0: {}",
        sel.start_m
    );
    assert_eq!(sel.snap_hint, Some("section edge"));
    c.exposure_pointer_up();

    // Grab the END handle (at the wall end) and drag it just short of the
    // corner — it snaps back to the wall's full length.
    c.exposure_pointer_down(b0.x as f64, b0.y as f64, 0.2);
    let near_end = along(len - 0.15);
    c.exposure_pointer_drag(near_end.0 as f64, near_end.1 as f64, 0.2);
    let sel2 = c.exposure_selection().unwrap();
    assert!(
        (sel2.end_m - len).abs() < 1e-3,
        "snapped to the wall end: {}",
        sel2.end_m
    );
    assert_eq!(sel2.snap_hint, Some("wall end"));
    c.exposure_pointer_up();
}

/// Exposure is reconstructed on reload from the persisted `WallRegions`
/// (the single source of truth) — NOT duplicated into `sketch_state`. A
/// marked wall comes back; un-marked walls stay unset.
#[test]
fn exposure_reconstructs_from_wall_regions_on_reload() {
    let mut c = dimensioned_square_controller();
    let (_, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
    c.set_exposure_mode(true);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_wall_exposure(WallExposure::NonHeatLoss); // mark only one wall

    c.materialize(); // edits defer the derive; bring current_record current
    let storey = c.current_record.as_ref().unwrap().data.storeys[0].clone();
    c.document.wall_sections.clear();
    c.load_storey_into_editor(Some(&storey));
    // Vertex ids are reassigned on reload (rebuilt from points), so check by
    // count: exactly one wall reconstructed, the rest unset.
    assert_eq!(
        c.document.wall_sections.len(),
        1,
        "one wall marked; rest unset"
    );
    let secs = c.document.wall_sections.values().next().unwrap();
    assert_eq!(secs.len(), 1);
    assert_eq!(secs[0].exposure, WallExposure::NonHeatLoss);
}

/// A split wall's sections reconstruct exactly from its regions on reload.
#[test]
fn split_wall_sections_reconstruct_on_reload() {
    let mut c = dimensioned_square_controller();
    let (_, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
    let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();
    c.set_exposure_mode(true);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_exposure_section_range(len * 0.2, len * 0.5);
    c.set_wall_exposure(WallExposure::SemiExposed); // → 1 painted section

    c.materialize(); // edits defer the derive; bring current_record current
    let storey = c.current_record.as_ref().unwrap().data.storeys[0].clone();
    c.document.wall_sections.clear();
    c.load_storey_into_editor(Some(&storey));
    // Only the PAINTED section comes back: the Unset gap regions around it
    // (and the whole-wall Unset regions) recover to no sections at all —
    // exactly the in-session state (single representation, no canonicalize).
    assert_eq!(
        c.document.wall_sections.len(),
        1,
        "one split wall reconstructed"
    );
    let secs = c.document.wall_sections.values().next().unwrap();
    assert_eq!(secs.len(), 1);
    assert_eq!(secs[0].exposure, WallExposure::SemiExposed);
    assert!(
        (secs[0].start_m - len * 0.2).abs() < 0.02,
        "{}",
        secs[0].start_m
    );
    assert!(
        (secs[0].end_m - len * 0.5).abs() < 0.02,
        "{}",
        secs[0].end_m
    );
}

/// "Set remaining → Exposed" paints every UNSET coverage range: whole
/// unmarked walls AND the unset gaps around painted sections, leaving the
/// painted sections (semi/non) untouched.
#[test]
fn set_all_remaining_fills_unset_walls_and_gaps() {
    let mut c = dimensioned_square_controller();
    let walls = c.document.outline.floor_plan.perimeter_walls();
    let (vid0, a0, b0) = walls[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
    let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();
    // Mark only the MIDDLE of wall0 — its two ends stay unset gaps.
    c.set_exposure_mode(true);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_exposure_section_range(len * 0.2, len * 0.5);
    c.set_wall_exposure(WallExposure::SemiExposed);

    c.set_all_remaining_exposed();
    assert_eq!(
        c.document.wall_sections.len(),
        walls.len(),
        "every wall now set"
    );
    // Wall0: the mark survives, its two unset GAPS were painted Exposed
    // (mandatory for the post-reload case, where gaps are the only Unset
    // representation).
    let secs = c.document.wall_sections.get(&vid0).unwrap();
    assert_eq!(secs.len(), 3, "gap + mark + gap, all explicit now");
    assert_eq!(secs[0].exposure, WallExposure::Exposed);
    assert_eq!(secs[1].exposure, WallExposure::SemiExposed);
    assert_eq!(secs[2].exposure, WallExposure::Exposed);
    for (vid, _, _) in &walls {
        if *vid != vid0 {
            assert_eq!(
                c.document.wall_sections.get(vid).map(|s| s[0].exposure),
                Some(WallExposure::Exposed)
            );
        }
    }
}

/// Assigning an exposure is undoable.
#[test]
fn undo_reverts_an_exposure_marking() {
    let mut c = dimensioned_square_controller();
    let (_, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
    c.set_exposure_mode(true);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    assert!(c.document.wall_sections.is_empty());
    c.set_wall_exposure(WallExposure::NonHeatLoss);
    assert_eq!(c.document.wall_sections.len(), 1);
    c.undo().unwrap();
    assert!(
        c.document.wall_sections.is_empty(),
        "undo clears the marking"
    );
}

/// Guardrail: a dimension that would push a living area OUTSIDE the storey
/// is rejected (and rolled back), like an invalid-shape dimension.
#[test]
fn dimension_pushing_living_area_outside_is_rejected() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    // 10m x 10m storey, dimensioned.
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(
        &[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.05, 0.05),
        ],
        0.1,
        true,
    )
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();

    c.set_living_area_mode(true);
    // ~2m room near the right edge (x in [6,8]).
    c.apply_living_area_stroke_event(&[(6.0, 3.0), (8.0, 3.0), (8.0, 5.0), (6.0, 5.0), (6.1, 3.1)]);
    assert_eq!(c.living_area_count(), 1);
    assert!(c.dimensioning_active());
    let before = c.document.floor_plan.polygons()[0].clone();
    // 9m wall on a room that starts at x=6 / y=3 would reach past the 10m
    // storey — must be rejected, and the shape left unchanged.
    let r = c.apply_dimension(9.0);
    assert!(
        r.is_err(),
        "a dimension pushing the room outside the storey is rejected: {r:?}"
    );
    assert_eq!(
        c.document.floor_plan.polygons()[0],
        before,
        "the rejected dimension is rolled back"
    );
}

/// A carved living area must be — and STAY — orthogonal: every edge
/// horizontal or vertical, right after carving and through dimensioning.
/// (A slanted edge here is the "left line isn't straight" report.)
#[test]
fn carved_living_area_stays_orthogonal_through_dimensioning() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();

    c.set_living_area_mode(true);
    c.apply_living_area_stroke_event(&[(0.0, 4.0), (4.0, 4.0), (4.0, 0.0)]);
    c.choose_cut_region(2.0, 2.0); // edge-to-edge stroke → pick the corner region
    assert_eq!(c.living_area_count(), 1);
    assert!(
        c.document.floor_plan.non_orthogonal_edges().is_empty(),
        "carved living area orthogonal right after carve: {:?}",
        c.document.floor_plan.non_orthogonal_edges()
    );

    let mut guard = 0;
    while c.dimensioning_active() && guard < 20 {
        guard += 1;
        let _ = c.apply_dimension(2.5);
        assert!(
            c.document.floor_plan.non_orthogonal_edges().is_empty(),
            "still orthogonal after dimension #{guard}: {:?}",
            c.document.floor_plan.non_orthogonal_edges()
        );
    }
}

/// The curve tools defer: drawing the arc only splits the wall (no shape
/// change); the user then dimensions the two attach stubs through the normal
/// prompt flow — only axis-aligned stubs are ever offered, never a facet —
/// and ONLY THEN is the circular arc faceted into the plan.
#[test]
fn curve_arc_is_provisional_until_attach_lines_are_dimensioned() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    let area0 = ring_area(&c);

    c.set_edit_mode(EditTools::ExtendCurve);
    // An arc bulging out of the top edge.
    let arc = vec![
        Point2f32::new(3.0, 10.0),
        Point2f32::new(3.6, 11.0),
        Point2f32::new(4.3, 11.7),
        Point2f32::new(5.0, 12.0),
        Point2f32::new(5.7, 11.7),
        Point2f32::new(6.4, 11.0),
        Point2f32::new(7.0, 10.0),
    ];
    c.apply_extend_stroke(&arc).expect("begin provisional arc");

    // Provisional: arc held, shape UNCHANGED (no facets, area the same), but
    // a preview is available and a dimension prompt is waiting.
    assert!(
        c.document.pending_arc.is_some(),
        "arc should be held provisional"
    );
    assert_eq!(diag_edges(&c), 0, "no facets while provisional");
    assert!(
        (ring_area(&c) - area0).abs() < 0.5,
        "begin must not change the shape"
    );
    assert!(c.pending_arc_preview().is_some(), "preview available");
    assert!(
        c.dimensioning_active(),
        "the attach stubs should be prompted"
    );

    // Dimension every prompted wall: each must be an axis-aligned stub, never
    // a facet. Two stubs get set; the arc stays provisional (not auto-baked).
    let mut dimensioned = 0;
    for _ in 0..10 {
        let Some(prompt) = c.current_dimension_prompt() else {
            break;
        };
        let (ring, edge) = c
            .document
            .floor_plan
            .find_vertex(prompt.wall_start_vertex)
            .unwrap();
        assert!(
            c.document.floor_plan.wall_axis(ring, edge).is_some(),
            "a curve facet must never be offered for dimensioning"
        );
        c.apply_dimension(2.0).expect("dimension an attach stub");
        dimensioned += 1;
    }
    assert_eq!(
        dimensioned, 2,
        "exactly the two attach stubs are dimensioned"
    );

    // Attach lines set, but the curve is NOT committed yet — it waits for
    // the radius to be accepted. The radius slider is now available, and
    // it's still a straight provisional placeholder (no facets).
    assert!(c.document.pending_arc.is_some(), "arc not auto-committed");
    assert_eq!(diag_edges(&c), 0, "still a straight placeholder pre-commit");
    let bulge0 = c
        .pending_arc_bulge()
        .expect("radius slider is available once attach lines are set");

    // Adjust the radius (slider), then accept.
    c.set_pending_arc_bulge(bulge0 + 0.3);
    assert!((c.pending_arc_bulge().unwrap() - (bulge0 + 0.3)).abs() < 1e-4);
    c.commit_pending_arc().expect("commit the curve");

    // Now baked: facets present, area grew, valid simple polygon.
    assert!(
        c.document.pending_arc.is_none(),
        "arc committed after accepting radius"
    );
    assert!(c.document.floor_plan.validate_simple().is_ok());
    assert!(diag_edges(&c) >= 2, "arc faceted after commit");
    assert!(
        ring_area(&c) > area0 + 1.0,
        "outward arc adds area once baked"
    );

    // The baked arc's facet params must be LOCKED (known). Otherwise a later
    // dimension elsewhere would treat them as free and `repair` /
    // `aesthetic_rescale` could redistribute the facet vertices — flattening
    // the curve back into a straight line (the reported extend-curve bug).
    let active = c.document.floor_plan.all_active_params();
    assert!(
        active
            .iter()
            .all(|p| c.document.dimensioning.known_params.contains(p)),
        "every facet param of a baked arc must be locked into known_params"
    );

    // Concretely: a subsequent aesthetic rescale (which only ever touches
    // un-set params) must leave the curved shape untouched.
    let area_before_rescale = ring_area(&c);
    let diag_before = diag_edges(&c);
    // (`document` derefs to the active storey, so the disjoint floor_plan /
    // dimensioning borrows must be split — lift the known set into a local.)
    let known = c.document.dimensioning.known_params.clone();
    c.document.floor_plan.aesthetic_rescale(&known);
    assert_eq!(diag_edges(&c), diag_before, "rescale must not drop facets");
    assert!(
        (ring_area(&c) - area_before_rescale).abs() < 1e-3,
        "a locked arc must survive aesthetic rescale unchanged"
    );
}

// ----- multi-storey navigation -----------------------------------------

/// Drop a dimensioned 10x10 square outline into whichever storey is current.
/// Used by the multi-storey tests to set each storey up identically.
fn finalise_current_storey_as_square(c: &mut AppController, side: f32) {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(
        &[
            p(0., 0.),
            p(side, 0.),
            p(side, side),
            p(0., side),
            p(0.05, 0.05),
        ],
        0.1,
        true,
    )
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
}

/// `add_storey` appends a fresh empty storey and moves the editor onto it.
#[test]
fn add_storey_appends_and_switches() {
    let mut c = dimensioned_square_controller();
    assert_eq!(c.current_storey_idx(), Some(0));
    assert_eq!(c.current_record.as_ref().unwrap().data.storeys.len(), 1);

    c.add_storey().expect("add_storey");
    assert_eq!(c.current_storey_idx(), Some(1), "switched onto new storey");
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys.len(),
        2,
        "new storey appended"
    );
    // The new storey is empty (no outline, no living areas, no walls); the
    // editor scale resets so the first dimension calibrates afresh.
    assert_eq!(
        c.document.floor_plan.ring_count(),
        0,
        "editor on a blank storey"
    );
    assert!(
        !c.document.dimensioning.scale_set,
        "scale resets so the new storey calibrates fresh"
    );
    assert_eq!(c.living_area_count(), 0);
    assert!(c.document.wall_sections.is_empty());
}

/// Switching storeys round-trips each storey's geometry through the editor:
/// dimensioning the first, adding & dimensioning the second, then switching
/// back leaves both intact in `current_record.data.storeys`.
#[test]
fn switching_storeys_preserves_each_storeys_geometry() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10
    let storey0_polys = c.current_record.as_ref().unwrap().data.storeys[0]
        .floors
        .to_vertex_lists();
    assert!(!storey0_polys.is_empty(), "storey 0 has a polygon");

    c.add_storey().unwrap();
    finalise_current_storey_as_square(&mut c, 6.0); // storey 1 = 6x6
    let storeys_after = c.current_record.as_ref().unwrap().data.storeys.clone();
    assert_eq!(storeys_after.len(), 2);
    let storey1_polys = storeys_after[1].floors.to_vertex_lists();
    assert!(!storey1_polys.is_empty(), "storey 1 has a polygon");

    // Switch back to storey 0 — its previously-committed geometry survives.
    c.set_current_storey(Some(0)).unwrap();
    assert_eq!(c.current_storey_idx(), Some(0));
    let storeys_back = c.current_record.as_ref().unwrap().data.storeys.clone();
    assert_eq!(storeys_back.len(), 2, "no storey lost on switch");
    assert_eq!(
        storeys_back[0].floors.to_vertex_lists(),
        storey0_polys,
        "storey 0's footprint preserved"
    );
    assert_eq!(
        storeys_back[1].floors.to_vertex_lists(),
        storey1_polys,
        "storey 1's footprint preserved"
    );
    // The editor now mirrors storey 0 again.
    assert_eq!(c.document.floor_plan.ring_count(), 1);
    assert!(c.document.dimensioning.scale_set);
}

/// All-storeys tab: `set_current_storey(None)` leaves the editor in a
/// read-only state — the live floor_plan is empty (no shape to edit on)
/// and the storeys list is preserved.
#[test]
fn switching_to_all_storeys_clears_editor_and_keeps_data() {
    let mut c = dimensioned_square_controller();
    c.add_storey().unwrap();
    finalise_current_storey_as_square(&mut c, 6.0);
    let snapshot = c.current_record.as_ref().unwrap().data.storeys.clone();

    c.set_current_storey(None).unwrap();
    assert_eq!(c.current_storey_idx(), None);
    assert_eq!(
        c.document.floor_plan.ring_count(),
        0,
        "no live shape on All tab"
    );
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys.len(),
        snapshot.len(),
        "every storey still present"
    );
    // Returning to a specific storey rehydrates the editor from its geometry.
    c.set_current_storey(Some(1)).unwrap();
    assert_eq!(c.document.floor_plan.ring_count(), 1);
}

/// Method-2 snap: on the FIRST stroke of a new upper storey, freehand points
/// drawn near the previous storey's outline get pulled onto it. We check
/// the helper directly (the stroke→pixel canvas conversion in
/// `apply_drawperp_stroke` is wrapped around a 10-unit RDP simplification
/// tolerance — large-scale coords keep the corners alive through that).
#[test]
fn first_stroke_on_new_storey_snaps_to_previous_outline() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10
    c.add_storey().unwrap();
    // Endpoints near (but not on) the previous outline's corners. The
    // snap tolerance scales to the previous diagonal (~6%), so 10x10 →
    // tol ≈ 0.85; any point within that lands on the outline exactly.
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let stroke = vec![
        p(0.2, 0.2), // near (0, 0) — snaps to (0, 0)
        p(9.8, 0.2), // near (10, 0) — snaps to (10, 0)
        p(9.8, 5.0), // near right wall at y=5 — snaps to x=10
        p(9.8, 9.8), // near (10, 10) — snaps to (10, 10)
        p(0.2, 9.8), // near (0, 10) — snaps to (0, 10)
        p(0.3, 0.3), // near (0, 0)
    ];
    let snapped = c
        .snap_to_previous_storey_outline(&stroke)
        .expect("snap helper engaged");
    // Endpoints aligned to the previous outline's corners exactly.
    let approx = |a: f32, b: f32| (a - b).abs() < 1e-3;
    assert!(approx(snapped[1].x, 10.0), "{:?}", snapped[1]);
    assert!(approx(snapped[1].y, 0.0));
    assert!(approx(snapped[3].x, 10.0), "{:?}", snapped[3]);
    assert!(approx(snapped[3].y, 10.0));
    // Mid-wall point pulled onto x=10, leaving y untouched.
    assert!(approx(snapped[2].x, 10.0), "{:?}", snapped[2]);
    assert!(approx(snapped[2].y, 5.0));
}

/// Without a previous storey to snap to, the helper returns `None`. Storey 0
/// has no watermark.
#[test]
fn first_stroke_on_storey_0_does_not_snap() {
    let c = dimensioned_square_controller(); // storey 0 only
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let stroke = vec![p(0.2, 0.2), p(9.7, 0.2)];
    // On storey 0 there's no previous outline → helper returns None.
    assert!(c.snap_to_previous_storey_outline(&stroke).is_none());
}

/// A new upper storey drawn (in world coords) tracing the SAME footprint as
/// the storey below inherits scale and pins every shared wall — so it's
/// fully resolved with no dimension prompt.
#[test]
fn outline_world_inherits_scale_and_pins_shared_walls() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10 (metres)
    c.add_storey().unwrap();
    // Trace the 10x10 freehand-style (a little off each corner) so the snap
    // pulls the endpoints onto the previous outline.
    let world = vec![(0.2, 0.2), (9.8, 0.2), (9.8, 9.8), (0.2, 9.8), (0.3, 0.3)];
    c.apply_outline_world_inner(&world, true)
        .expect("apply world outline");

    assert_eq!(c.document.floor_plan.ring_count(), 1, "outline built");
    assert!(
        c.document.dimensioning.scale_set,
        "geometry is already metres → scale inherited"
    );
    assert!(
        !c.document.dimensioning.known_params.is_empty(),
        "shared walls pinned"
    );
    // Footprint ≈ the 10x10 below.
    let area = ring_area(&c);
    assert!(
        (area - 100.0).abs() < 2.0,
        "footprint ≈ 100 m² (got {area})"
    );
    // Every wall coincides with the storey below → nothing left to ask.
    assert!(
        c.document.dimensioning.current_prompt.is_none(),
        "all walls shared → no dimension prompt"
    );
}

/// The new-storey WORLD outline (the carve path) records its elective snaps
/// so they surface as tag overlays, and replays as `SnapAction::OutlineWorld`
/// when the user un-snaps or picks an alternative.
#[test]
fn outline_world_stroke_is_captured_for_snap_replay() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10 (metres)
    c.add_storey().unwrap();
    // The new-storey flow asks the slab thickness FIRST (outline strokes are
    // gated until it lands); answer it so the draw path opens.
    c.assign_floor_thickness(0.3).unwrap();
    // The engine's pixel stroke only supplies the close-check; the world
    // points below are what get applied.
    c.sketch_start_stroke(Point2f32::new(0.0, 0.0)).unwrap();
    for (x, y) in [(100.0, 0.0), (100.0, 100.0), (0.0, 100.0), (1.0, 1.0)] {
        c.sketch_add_point(Point2f32::new(x, y)).unwrap();
    }
    // Trace the 10x10 a little off each corner so the carve's corner snaps fire.
    let world = vec![(0.2, 0.2), (9.8, 0.2), (9.8, 9.8), (0.2, 9.8), (0.3, 0.3)];
    c.apply_outline_stroke_world(&world);
    assert_eq!(c.document.floor_plan.ring_count(), 1, "outline built");
    let tags = c.snap_overlay();
    assert!(
        !tags.is_empty(),
        "the carve's elective snaps surface as clickable tags"
    );
    // Un-snap replays the SAME world stroke with every snap disabled.
    c.unsnap_last();
    assert_eq!(
        c.document.floor_plan.ring_count(),
        1,
        "replay rebuilt the outline"
    );
    let tags = c.snap_overlay();
    assert!(!tags.is_empty(), "replay re-captured its decisions");
    assert!(
        tags.iter().all(|t| t.current_kind.is_none()),
        "all snaps disabled after un-snap"
    );
}

/// A storey ~5 m across with a curved LEFT wall and a bulged RIGHT side.
/// Used to check the new-storey reproduce/splice decision at a realistic
/// small scale (where the snap radius is a large fraction of the plan).
fn bulged_storey_controller() -> AppController {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    c.document.floor_plan = FloorPlan::from_clean_ring(&[
        p(0., 0.),
        p(3., 0.),
        p(3.5, 1.5), // right side bulges OUT to x=3.5
        p(3., 3.),
        p(0., 3.),
        p(-0.2, 2.2), // left side is a 3-facet curve bulging out
        p(-0.3, 1.5),
        p(-0.2, 0.8),
    ])
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
    c
}

/// A second storey that runs a STRAIGHT wall where the storey below bulges
/// out (a ~0.5 m setback) must NOT be mistaken for a faithful reproduction —
/// even though the setback is small relative to the snap radius. The curve on
/// the OTHER side, which the trace did follow, is preserved (spliced
/// verbatim, not squared). A genuinely faithful trace still reproduces.
#[test]
fn second_storey_setback_over_a_bulge_is_not_a_reproduce() {
    let sample = |a: (f32, f32), b: (f32, f32), out: &mut Vec<(f32, f32)>| {
        let steps = 30;
        for s in 0..steps {
            let f = s as f32 / steps as f32;
            out.push((a.0 + f * (b.0 - a.0), a.1 + f * (b.1 - a.1)));
        }
    };
    // Faithful trace: follow every wall, bulge and curve included.
    let mut faithful = Vec::new();
    for (a, b) in [
        ((0., 0.), (3., 0.)),
        ((3., 0.), (3.5, 1.5)),
        ((3.5, 1.5), (3., 3.)),
        ((3., 3.), (0., 3.)),
        ((0., 3.), (-0.2, 2.2)),
        ((-0.2, 2.2), (-0.3, 1.5)),
        ((-0.3, 1.5), (-0.2, 0.8)),
        ((-0.2, 0.8), (0., 0.)),
    ] {
        sample(a, b, &mut faithful);
    }
    faithful.push((0., 0.));

    let mut cf = bulged_storey_controller();
    let area0 = ring_area(&cf);
    cf.add_storey().unwrap();
    cf.apply_outline_world_inner(&faithful, true).unwrap();
    assert!(
        (ring_area(&cf) - area0).abs() < 0.2,
        "a faithful trace reproduces the storey below verbatim"
    );

    // Setback trace: identical EXCEPT the right side is a straight wall at
    // x=2.5 — a MID-span setback (not at the storey-below's corners), cutting
    // off the bulge.
    let mut setback = Vec::new();
    for (a, b) in [
        ((0., 0.), (2.5, 0.)),
        ((2.5, 0.), (2.5, 3.)), // straight, set back from the bulge
        ((2.5, 3.), (0., 3.)),
        ((0., 3.), (-0.2, 2.2)),
        ((-0.2, 2.2), (-0.3, 1.5)),
        ((-0.3, 1.5), (-0.2, 0.8)),
        ((-0.2, 0.8), (0., 0.)),
    ] {
        sample(a, b, &mut setback);
    }
    setback.push((0., 0.));

    let mut cs = bulged_storey_controller();
    cs.add_storey().unwrap();
    cs.apply_outline_world_inner(&setback, true).unwrap();
    // The mid-span setback wall is NEW — its position must be queried, not
    // accepted as drawn (the parametric origin must land on a shared corner,
    // not the new wall, or its position freezes as a constant).
    assert!(
        cs.document.dimensioning.current_prompt.is_some(),
        "the new set-back wall's position is offered for dimensioning"
    );
    let pts = cs.document.floor_plan.get_points();
    // The bulge apex is gone — the right wall was straightened.
    assert!(
        !pts.iter().any(|q| (q.x - 3.5).abs() < 0.2),
        "the bulge the trace cut off is not reproduced"
    );
    // The LEFT curve the trace DID follow is preserved (diagonal facets kept).
    assert!(
        !cs.document.floor_plan.non_orthogonal_edges().is_empty(),
        "the followed left curve is spliced verbatim, not squared off"
    );
    assert!(
        ring_area(&cs) < area0 - 0.3,
        "cutting the bulge reduced the footprint (got {} vs {area0})",
        ring_area(&cs)
    );
}

/// The core feature: an upper storey that extends past the storey below
/// (a bump-out over a ground-floor garage). The walls it shares with the
/// storey below are pinned; the genuinely-new garage walls are NOT, so the
/// assessor is still prompted to dimension them.
#[test]
fn garage_extension_leaves_new_wall_unpinned() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10
    c.add_storey().unwrap();
    // Hug three sides of the 10x10, but bump out a 3x3 garage past x=10
    // between y=4 and y=7. Shared-wall points are a touch off (snap pulls
    // them in); garage corners are exact and well outside the snap radius.
    let world = vec![
        (0.2, 0.2),  // → (0,0)
        (9.8, 0.2),  // → (10,0)
        (9.8, 4.0),  // → x=10 on the right wall
        (13.0, 4.0), // garage corner (new)
        (13.0, 7.0), // garage corner (new)
        (9.8, 7.0),  // → x=10 on the right wall
        (9.8, 9.8),  // → (10,10)
        (0.2, 9.8),  // → (0,10)
        (0.3, 0.3),  // → (0,0)
    ];
    c.apply_outline_world_inner(&world, true)
        .expect("apply world outline");

    assert_eq!(c.document.floor_plan.ring_count(), 1);
    assert!(c.document.dimensioning.scale_set);
    assert!(
        !c.document.dimensioning.known_params.is_empty(),
        "shared walls pinned"
    );
    // Footprint is bigger than the 100 m² below (gained the garage bump).
    let area = ring_area(&c);
    assert!(area > 105.0, "extended footprint > 100 m² (got {area})");
    // The genuinely-new (garage) walls are NOT pinned: some active param is
    // still unknown, so the assessor will be asked to dimension it (after
    // any collinearity review that the new corners trigger).
    let active = c.document.floor_plan.all_active_params();
    assert!(
        active
            .iter()
            .any(|p| !c.document.dimensioning.known_params.contains(p)),
        "a new (garage) wall's parameter is left unpinned to dimension \
         (active={}, known={})",
        active.len(),
        c.document.dimensioning.known_params.len()
    );
}

/// Collinearity separations scale to the plan's units: a clean 10x10 *metre*
/// square has its opposite walls 10 m apart — far beyond the near-collinear
/// range — so nothing is flagged. (With the old fixed pixel range a 10 m gap
/// looked "near-collinear" and every parallel wall pair was asked about.)
#[test]
fn collinearity_range_scales_to_metre_plan() {
    let mut c = dimensioned_square_controller(); // 10x10 metre square
    c.review = None;
    c.populate_collinearity_review();
    assert!(
        c.review.is_none(),
        "a clean metre-scale square must not flag every wall as collinear"
    );
}

/// Accepting a collinearity decision drops the now-redundant vertex. A
/// spurious "step" (from mouse jitter) flagged near-collinear collapses to a
/// clean wall once accepted — the straight-through vertices are removed, so
/// the user is never asked for a length that can't be set without changing
/// the shape. Runs with `scale_set = true` (the storey is already in metres,
/// as an upper storey is) so the `simplify_degenerate` safety net is skipped
/// and the cleanup must collapse the zero-length step edge itself.
#[test]
fn accepting_collinearity_removes_spurious_step_vertices() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    // A 10x10 whose bottom wall has a spurious 0.3 m step (two horizontals
    // at y=0 and y=0.3 joined by a tiny vertical) — six vertices.
    c.document.floor_plan = FloorPlan::from_clean_ring(&[
        p(0., 0.),
        p(5., 0.),
        p(5., 0.3),
        p(10., 0.3),
        p(10., 10.),
        p(0., 10.),
    ])
    .unwrap();
    // Already metric (like an upper storey drawn in world coords): scale set,
    // every current param known. This is the path where `simplify_degenerate`
    // is NOT run, so only the collinear/coincident cleanup can remove the step.
    c.document.dimensioning = DimensionState::default();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    assert_eq!(
        c.document.floor_plan.polygons()[0].len(),
        6,
        "starts with the step"
    );

    c.populate_collinearity_review();
    assert!(c.review.is_some(), "the near-collinear step is flagged");

    // Accept every pending suggestion (the step's two horizontals = 1 pair).
    while c.review.is_some() {
        c.accept_current_review().unwrap();
    }

    // The step's redundant vertices are gone → back to a clean 4-corner box.
    assert_eq!(
        c.document.floor_plan.polygons()[0].len(),
        4,
        "spurious step vertices removed after collinearity accept"
    );
    let area = ring_area(&c);
    assert!(
        (area - 100.0).abs() < 1.0,
        "outline is a clean 10x10 (got {area})"
    );
}

/// Reproduces the field report: a new upper storey traced over the one below
/// with an exaggerated "nib" bumping off the base (a slipped pen). Two fixes
/// are exercised together:
///   1. The near-collinear horizontals collapse onto the PREVIOUS storey's
///      base (y=0), not the nib's offset line (y=1.2) — so the footprint
///      stays at ~100 m² rather than shrinking to ~88.
///   2. Once that merge collapses the nib's connectors to zero length, the
///      review's leftover vertical suggestion about them is auto-skipped, so
///      the user isn't asked about a line that no longer exists.
#[test]
fn nib_collapses_onto_previous_base_and_skips_degenerate_connector() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10, base y=0
    c.add_storey().unwrap();
    // Base mostly on y=0, with a 0.3-wide nib bumping up to y=1.2 between
    // x=4.0 and x=4.3. The 1.2 m bump clears the snap tolerance (so it
    // survives as a near-collinear question) but stays within range.
    let world = vec![
        (0.0, 0.0),
        (4.0, 0.0),
        (4.0, 1.2),
        (4.3, 1.2),
        (4.3, 0.0),
        (10.0, 0.0),
        (10.0, 10.0),
        (0.0, 10.0),
        (0.05, 0.05),
    ];
    c.apply_outline_world_inner(&world, true)
        .expect("apply world outline");

    let n = c.review.as_ref().map_or(0, |r| r.suggestions.len());
    assert!(
        n >= 2,
        "the nib creates a horizontal (base/top) and a vertical (connectors) pair (got {n})"
    );

    // Accept the first (horizontal) pair.
    c.accept_current_review().unwrap();
    // The connectors are now zero-length, so the remaining vertical
    // suggestion about them is auto-skipped → the review is already done.
    assert!(
        c.review.is_none(),
        "degenerate-connector suggestion auto-skipped after the merge"
    );

    // Nib gone, clean 4-corner box, and crucially the base sat DOWN onto
    // y=0 (area ~100) instead of UP onto the nib line (which would give ~88).
    assert_eq!(
        c.document.floor_plan.polygons()[0].len(),
        4,
        "nib removed, clean box"
    );
    let area = ring_area(&c);
    assert!(
        (area - 100.0).abs() < 1.0,
        "collapsed onto the previous base (y=0) → ~100 m² (got {area})"
    );
}

/// Sample a vertex ring into a DENSE closed trace (~`step` spacing), the way
/// a real freehand outline stroke arrives — hundreds of points, not just the
/// corners. The new-storey reproduce/coverage check measures how closely the
/// trace hugs the boundary, which only behaves like real input when the trace
/// is sampled, not corner-to-corner.
fn densify_ring(verts: &[Point2f32], step: f32) -> Vec<(f32, f32)> {
    let n = verts.len();
    let mut out = Vec::new();
    for i in 0..n {
        let (a, b) = (verts[i], verts[(i + 1) % n]);
        let len = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
        let nsamp = (len / step).ceil().max(1.0) as usize;
        for s in 0..nsamp {
            let f = s as f32 / nsamp as f32;
            out.push((a.x + f * (b.x - a.x), a.y + f * (b.y - a.y)));
        }
    }
    out.push((verts[0].x, verts[0].y));
    out
}

/// Building a new storey by tracing over a storey that has a CURVED wall
/// copies that storey's exact (faceted) linear representation up — the curve
/// is preserved, not squared off into right angles. Reproduces the manual
/// session: storey 0 has a 3-facet rounded corner; the new storey traces the
/// same outline.
#[test]
fn new_storey_over_curved_storey_copies_curve_verbatim() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    // Storey 0: a 4x3 box whose top-right corner is rounded by 3 facets.
    let curved = vec![
        p(0., 0.),
        p(4., 0.),
        p(4., 2.0),
        p(3.7, 2.6),
        p(3.2, 2.9),
        p(2.5, 3.0),
        p(0., 3.0),
    ];
    c.document.floor_plan = FloorPlan::from_clean_ring(&curved).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
    let prev_nonortho = c.document.floor_plan.non_orthogonal_edges().len();
    assert!(
        prev_nonortho >= 2,
        "storey 0 has a curved (non-orthogonal) run"
    );

    c.add_storey().unwrap();
    // Trace the same outline DENSELY (as a real freehand stroke is), reproducing
    // it with no real excursion.
    let trace = densify_ring(&curved, 0.1);
    c.apply_outline_world_inner(&trace, true).unwrap();

    // The curve is carried up VERBATIM (not squared off): same number of
    // non-orthogonal facet edges, every param pinned, nothing to dimension.
    assert_eq!(
        c.document.floor_plan.non_orthogonal_edges().len(),
        prev_nonortho,
        "curve facets preserved in the new storey (not squared off)"
    );
    assert!(c.document.dimensioning.scale_set);
    assert!(
        c.document.dimensioning.current_prompt.is_none(),
        "whole outline shared → nothing to dimension"
    );
    let area = ring_area(&c);
    let prev_area = {
        let n = curved.len();
        let mut a = 0.0;
        for i in 0..n {
            let j = (i + 1) % n;
            a += curved[i].x * curved[j].y - curved[j].x * curved[i].y;
        }
        (a * 0.5).abs()
    };
    assert!(
        (area - prev_area).abs() < 0.5,
        "footprint matches the storey below (got {area}, expected {prev_area})"
    );
}

/// Full process, the part that was broken: a finished (dimensioned, curved)
/// first storey, then a second storey traced over it with realistic hand
/// SLOP — points that stray past the snap radius (so they don't snap exactly)
/// but stay within the new-wall tolerance. That's loose tracing, not a new
/// wall, so the curve must still be followed verbatim. (The old
/// max-distance reproduce check mistook the stray for an excursion and
/// squared the curve off.)
#[test]
fn second_storey_follows_curve_despite_trace_slop() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    // Finished first storey: a 4x3 box with a 3-facet rounded corner.
    let curved = vec![
        p(0., 0.),
        p(4., 0.),
        p(4., 2.0),
        p(3.7, 2.6),
        p(3.2, 2.9),
        p(2.5, 3.0),
        p(0., 3.0),
    ];
    c.document.floor_plan = FloorPlan::from_clean_ring(&curved).unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
    let curve_edges = c.document.floor_plan.non_orthogonal_edges().len();
    assert!(curve_edges >= 2, "first storey has the curved run");
    assert!(
        c.document.dimensioning.current_prompt.is_none(),
        "first storey fully dimensioned"
    );

    c.add_storey().unwrap();
    // Trace the SAME outline densely, but TWO isolated points stray ~0.7 m off
    // the boundary — past the snap radius (~0.4 m here), within the new-wall
    // tolerance (1 m), with on-boundary neighbours either side. Hand jitter,
    // not a sustained new wall, so the curve must still be followed verbatim.
    let mut trace = densify_ring(&curved, 0.1);
    let q = trace.len() / 4;
    trace[q] = (trace[q].0, trace[q].1 - 0.7); // a point on the bottom strays down
    let q3 = trace.len() * 3 / 4;
    trace[q3] = (trace[q3].0 - 0.7, trace[q3].1); // a point on the left strays out
    c.apply_outline_world_inner(&trace, true)
        .expect("apply world outline");

    // Curve followed verbatim; whole outline shared → nothing to dimension.
    assert_eq!(
        c.document.floor_plan.non_orthogonal_edges().len(),
        curve_edges,
        "the curve is followed despite slop (not squared off)"
    );
    assert!(
        c.document.dimensioning.current_prompt.is_none(),
        "whole outline shared → no dimension prompt"
    );
}

/// `resync_dimensioning_to_plan` (the consolidated post-topology-change
/// cleanup) drops dimensioning state that no longer refers to a live
/// param/vertex, and keeps the rest.
#[test]
fn resync_dimensioning_drops_stale_params_and_vertices() {
    let mut c = dimensioned_square_controller();
    let active = c.document.floor_plan.all_active_params();
    assert!(!active.is_empty(), "the square has active params");

    // Seed real state + bogus entries (ParameterId/VertexId are usize/u64).
    c.document.dimensioning.known_params = active.clone();
    c.document.dimensioning.known_params.insert(99_999); // not an active param
    c.document.dimensioning.dimensioned_walls = vec![123_456]; // not a live vertex
    c.document.dimensioning.skipped_walls = std::iter::once(123_456).collect();

    c.resync_dimensioning_to_plan();

    assert_eq!(
        c.document.dimensioning.known_params, active,
        "stale param pruned, real ones kept"
    );
    assert!(
        c.document.dimensioning.dimensioned_walls.is_empty(),
        "stale dimensioned wall pruned"
    );
    assert!(
        c.document.dimensioning.skipped_walls.is_empty(),
        "stale skipped wall pruned"
    );
}

/// A new upper storey starts zoomed out so the previous-storey watermark
/// leaves room to draw a larger footprint; storey 0 fits normally.
#[test]
fn add_storey_zooms_out() {
    let mut c = dimensioned_square_controller();
    assert_eq!(c.view().zoom, 1.0, "storey 0 fits normally");
    c.add_storey().unwrap();
    assert!(
        c.view().zoom < 1.0,
        "new storey zoomed out to leave drawing room (got {})",
        c.view().zoom
    );
}

/// Two-storey DB round-trip: both finalised storeys come back on reopen.
#[test]
fn two_storey_db_round_trip() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let ber = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    finalise_current_storey_as_square(&mut c, 10.0);
    c.add_storey().unwrap();
    finalise_current_storey_as_square(&mut c, 6.0);

    c.open_ber(&ber.id).expect("reload");
    let storeys = &c.current_record.as_ref().unwrap().data.storeys;
    assert_eq!(storeys.len(), 2, "both storeys reloaded");
    let s0 = storeys[0].floors.to_vertex_lists();
    let s1 = storeys[1].floors.to_vertex_lists();
    assert!(!s0.is_empty(), "storey 0 has a footprint");
    assert!(!s1.is_empty(), "storey 1 has a footprint");
    // Areas (signed shoelace, abs) — 10x10 ≈ 100, 6x6 ≈ 36.
    let area_of = |ring: &Vec<Point2f64>| {
        let mut a = 0.0;
        let n = ring.len();
        for i in 0..n {
            let j = (i + 1) % n;
            a += ring[i].x * ring[j].y - ring[j].x * ring[i].y;
        }
        (a * 0.5).abs()
    };
    let a0 = area_of(&s0[0]);
    let a1 = area_of(&s1[0]);
    assert!((a0 - 100.0).abs() < 1.0, "storey 0 ~ 100 m² (got {a0})");
    assert!((a1 - 36.0).abs() < 1.0, "storey 1 ~ 36 m² (got {a1})");
    // Reopen RESUMES the storey last edited (the snapshot's storey) — here
    // storey 1, finalised last — rather than always resetting to storey 0.
    assert_eq!(c.current_storey_idx(), Some(1));
}

#[test]
fn snapshot_version_mismatch_falls_back_to_rebuild() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let ber = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    finalise_current_storey_as_square(&mut c, 10.0);
    c.flush_if_dirty().unwrap();

    // Poison the snapshot: an incompatible version + unparseable JSON. The
    // loader must reject it and rebuild from the durable record's points.
    BerRepository::new(&c.conn)
        .save_snapshot(&ber.id, PARAMETRIC_SNAPSHOT_VERSION + 1, 0, "not json")
        .unwrap();

    c.open_ber(&ber.id)
        .expect("reopen falls back, doesn't error");
    assert!(
        !c.document.outline.floor_plan.get_points().is_empty(),
        "a stale/garbage snapshot is ignored; the outline is rebuilt from the durable BER"
    );
}

/// §6 + snapshot-shrink regression. Floor exposure is single-homed on Layer 1
/// (`skip_serializing`), so the snapshot no longer carries it and the load path
/// must SEED it back from the resolved regions. Assert a labelled ground floor
/// survives a normal reopen AND a subsequent re-resolve — the "floor goes red
/// after adding a window" bug. The same shared reconstruct+seed pass now runs on
/// the checkpoint path too, so this covers that fix as well.
#[test]
fn floor_exposure_survives_reload_and_edit_via_layer1_seed() {
    use ber_model::ber::structural::FloorExposure;
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let ber = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    finalise_current_storey_as_square(&mut c, 10.0);
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();

    // Manually label the ground floor (auto-derive leaves the lowest floor Unset).
    let fkey = document::centroid_key(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .floors
            .regions[0]
            .centroid(),
    );
    c.document.storeys[0]
        .floor_exposure
        .insert(fkey, FloorExposure::Ground);
    c.commit_to_ber().unwrap();
    c.flush_if_dirty().unwrap();

    // Reopen. The snapshot no longer carries floor_exposure, so the working map
    // is empty after restore — the seed must repopulate it from the Ground region.
    c.open_ber(&ber.id).unwrap();
    assert!(
        !c.document.storeys[0].floor_exposure.is_empty(),
        "floor_exposure seeded from Layer 1 on load (snapshot omits it)"
    );
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .floors
            .regions[0]
            .exposure,
        FloorExposure::Ground,
        "exposure intact right after reload"
    );

    // The §6 trigger: an edit re-resolves; the seeded map keeps it Ground (no red).
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .floors
            .regions[0]
            .exposure,
        FloorExposure::Ground,
        "exposure survives a re-resolve after reload (no green→red regression)"
    );
}

/// The 3D gate: the UI's 2D/3D toggle is disabled until the scope's heights
/// are set; the controller guard is defense in depth — requesting 3D before
/// the current storey has a ceiling height is a SILENT NO-OP (stays 2D).
#[test]
fn requesting_3d_without_ceiling_height_is_a_silent_noop() {
    let mut c = dimensioned_square_controller();
    assert_eq!(c.sketch_mode(), SketchMode::Storey2D);
    assert!(!c.ceiling_heights_set(), "no height entered yet");
    c.set_sketch_mode(SketchMode::Storey3D);
    assert_eq!(
        c.sketch_mode(),
        SketchMode::Storey2D,
        "refused until a height is entered via the Heights category"
    );
}

/// Setting the height (a Heights-category action) does NOT switch the view —
/// the editor stays in 2D. It opens the 3D gate (the switch now succeeds),
/// and the next materialize derives the ceilings at that height (one flat
/// region per outline ring) with the walls extruded to it.
#[test]
fn setting_ceiling_height_stays_2d_and_opens_the_3d_gate() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.7).expect("valid height");
    assert_eq!(
        c.sketch_mode(),
        SketchMode::Storey2D,
        "setting a height is not a view switch"
    );
    assert!(c.ceiling_heights_set());
    assert!(
        (c.default_ceiling_height().unwrap() - 2.7).abs() < 1e-6,
        "the Heights field shows the stored value"
    );

    c.set_sketch_mode(SketchMode::Storey3D);
    assert_eq!(c.sketch_mode(), SketchMode::Storey3D, "gate now open");

    c.materialize(); // edits defer the derive; bring current_record current
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(
        (storey.ceiling_height_m.unwrap() - 2.7).abs() < 1e-6,
        "height stamped on the storey"
    );
    assert_eq!(
        storey.ceilings.regions.len(),
        1,
        "one region per outline ring"
    );
    assert!(
        storey.ceilings.regions[0]
            .polygon
            .exterior
            .iter()
            .all(|v| (v.z - 2.7).abs() < 1e-6),
        "every roof vertex at the entered height"
    );
    // Walls re-extrude to the real height on the same materialize.
    assert!(storey
        .walls
        .regions
        .iter()
        .flat_map(|w| w.polygon.exterior.iter())
        .all(|q| q.z.abs() < 1e-6 || (q.z - 2.7).abs() < 1e-6));
}

/// Zero, negative and non-finite heights are rejected with a user-facing
/// error (shown inline next to the Heights field); nothing is stamped and
/// the 3D gate stays closed.
#[test]
fn invalid_ceiling_heights_rejected() {
    let mut c = dimensioned_square_controller();
    for bad in [0.0_f32, -1.0, f32::NAN] {
        assert!(
            c.set_default_ceiling_height(bad).is_err(),
            "{bad} must be rejected"
        );
        assert_eq!(c.sketch_mode(), SketchMode::Storey2D, "still 2D");
    }
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0].ceiling_height_m,
        None,
        "nothing stamped"
    );
    assert!(!c.ceiling_heights_set(), "3D gate still closed");
}

/// From the All-storeys tab the Heights field's scope is every storey: one
/// typed value fills only the storeys WITHOUT a height, never overwriting
/// one the assessor already set per-storey.
#[test]
fn all_storeys_ceiling_scope_fills_only_unset_storeys() {
    let mut c = dimensioned_square_controller();
    // Storey 0 gets its own height via the per-storey scope.
    c.set_default_ceiling_height(3.1).unwrap();
    // A fresh storey 1 has no height → the All-storeys scope is incomplete
    // and the full-house 3D switch is still refused there.
    c.add_storey().unwrap();
    c.set_current_storey(None).unwrap();
    assert!(!c.ceiling_heights_set(), "storey 1 has no height yet");
    c.set_sketch_mode(SketchMode::Storey3D);
    assert_eq!(c.sketch_mode(), SketchMode::Storey2D, "3D refused");
    assert_eq!(
        c.default_ceiling_height(),
        None,
        "mixed/incomplete house: no single value to show in the field"
    );

    c.set_default_ceiling_height(2.4).unwrap();
    let storeys = &c.current_record.as_ref().unwrap().data.storeys;
    assert!(
        (storeys[0].ceiling_height_m.unwrap() - 3.1).abs() < 1e-6,
        "already-set storey untouched"
    );
    assert!(
        (storeys[1].ceiling_height_m.unwrap() - 2.4).abs() < 1e-6,
        "gap filled with the typed value"
    );
    assert!(c.ceiling_heights_set(), "all-storeys scope now complete");
    c.set_sketch_mode(SketchMode::Storey3D);
    assert_eq!(c.sketch_mode(), SketchMode::Storey3D, "gate now open");
}

// ─── Stage A checklist (red / yellow / green roll-up) ───────────────────────

/// The Stage A checklist rolls each item up against the current scope: green
/// when every storey is complete, yellow when the current storey is complete
/// but not all, red otherwise. Measurements (dimensioning) demonstrates all
/// three; heights flips red→green when set.
#[test]
fn stage_a_checklist_rolls_up_red_yellow_green() {
    fn status(c: &AppController, label: &str) -> ChecklistStatus {
        c.stage_a_checklist()
            .into_iter()
            .find(|i| i.label == label)
            .expect("checklist item exists")
            .status
    }
    let mut c = dimensioned_square_controller();
    // The only storey is fully dimensioned → measurements complete everywhere.
    assert_eq!(status(&c, "Measurements"), ChecklistStatus::AllStoreys);
    // Heights + exposures untouched → red.
    assert_eq!(status(&c, "Heights"), ChecklistStatus::Incomplete);
    assert_eq!(status(&c, "Wall exposures"), ChecklistStatus::Incomplete);

    // Setting the height completes it on the only storey → green.
    c.set_default_ceiling_height(2.4).unwrap();
    assert_eq!(status(&c, "Heights"), ChecklistStatus::AllStoreys);

    // A fresh, undrawn storey: measurements now complete on storey 0 but not on
    // storey 1. `add_storey` switches onto the new storey, where the current
    // scope is incomplete → red.
    c.add_storey().unwrap();
    c.materialize();
    assert_eq!(status(&c, "Measurements"), ChecklistStatus::Incomplete);
    // Back on the finished storey 0 → complete here but not on all → yellow.
    c.set_current_storey(Some(0)).unwrap();
    c.materialize();
    assert_eq!(status(&c, "Measurements"), ChecklistStatus::ThisStorey);
    // Heights was only set on storey 0, so it too is now yellow.
    assert_eq!(status(&c, "Heights"), ChecklistStatus::ThisStorey);
}

// ─── Ceiling steps (line + side + amount) ───────────────────────────────────

/// A finalised 10×10 storey with its default ceiling height set (2.4 m) and
/// ceiling-step mode entered — the starting point of every step flow.
fn ceiling_ready_controller() -> AppController {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).expect("height applies");
    c.set_ceiling_step_mode(true);
    assert!(c.ceiling_step_mode(), "mode enters once available");
    c
}

/// Sorted flat heights of storey 0's derived roof regions.
fn ceiling_flat_zs(c: &AppController) -> Vec<f64> {
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let mut zs: Vec<f64> = storey
        .ceilings
        .regions
        .iter()
        .filter_map(|r| r.flat_z())
        .collect();
    zs.sort_by(f64::total_cmp);
    zs
}

/// Drive a fresh flow to the AMOUNT stage with the line pinned at exactly
/// x = 4 (vertical line, Greater side): stroke → side tap → position apply.
fn ceiling_step_to_amount_at_x4(c: &mut AppController) {
    c.apply_ceiling_step_stroke_event(&[(3.78, 0.5), (3.82, 5.0), (3.8, 9.5)]);
    assert!(c.ceiling_step_side_pending(), "stroke fitted → side pick");
    c.ceiling_step_tap(7.0, 5.0); // right of the line → Greater side
    let ui = c.position_prompt_for_ui().expect("line position prompt");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    assert_eq!(ui.axis, ParameterAxis::X);
    c.apply_position_dimension(4.0).expect("position applies"); // 4 m from x = 0
    assert!(c.ceiling_step_amount_pending());
}

/// The debug dump materializes first and reports both the session inputs and
/// the per-storey derivation in the stable greppable format.
#[test]
fn debug_dump_current_reports_session_state_and_storey() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).expect("delta applies");
    let dump = c.debug_dump_current();
    assert!(dump.contains("=== SESSION STATE ==="));
    assert!(dump.contains("ceiling_step=true"), "{dump}");
    assert!(dump.contains("ceiling_step_region[storey 0][0] delta_m=0.3000"));
    assert!(dump.contains("=== STOREY DUMP[0] ==="));
    assert!(dump.contains("ceiling_height_m: Some(2.4"), "{dump}");
    assert!(dump.contains("ceilings (2):"), "{dump}");
    assert!(dump.contains("=== END STOREY DUMP[0] ==="));
}

/// The mode toggle is gated: refused while the storey has no default ceiling
/// height (the deltas are relative to it), available once it's set, and never
/// available on the read-only All tab.
#[test]
fn ceiling_step_mode_gated_on_default_height() {
    let mut c = dimensioned_square_controller();
    assert!(!c.ceiling_step_available(), "no default height yet");
    c.set_ceiling_step_mode(true);
    assert!(!c.ceiling_step_mode(), "setter re-enforces the gate");

    c.set_default_ceiling_height(2.4).unwrap();
    assert!(c.ceiling_step_available());
    c.set_ceiling_step_mode(true);
    assert!(c.ceiling_step_mode());

    // The All tab is read-only — not available there (the switch also resets
    // the mode like the other area tools).
    c.set_current_storey(None).unwrap();
    assert!(!c.ceiling_step_available());
    assert!(!c.ceiling_step_mode());
}

/// The full stroke path: near-straight stroke → side tap → position prompt
/// (apply pins the line at exactly 4 m) → amount → materialize splits the
/// roof into h / h+Δ pieces either side of the line, and the session list
/// canonicalizes to the exact derived stepped piece.
#[test]
fn ceiling_step_stroke_side_position_amount_splits_roofs() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).expect("amount applies");
    assert!(!c.ceiling_step_pending(), "flow complete");

    c.materialize();
    let zs = ceiling_flat_zs(&c);
    assert_eq!(zs.len(), 2, "roof split into two flat pieces");
    assert!((zs[0] - 2.4).abs() < 1e-6, "default piece keeps h");
    assert!((zs[1] - 2.7).abs() < 1e-6, "stepped piece at h + Δ");
    // The RAISED piece is the one right of the line.
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    for region in &storey.ceilings.regions {
        let n = region.polygon.exterior.len() as f64;
        let cx = region.polygon.exterior.iter().map(|v| v.x).sum::<f64>() / n;
        let z = region.flat_z().expect("flat");
        if cx > 4.0 {
            assert!((z - 2.7).abs() < 1e-6, "right of the line steps up");
        } else {
            assert!((z - 2.4).abs() < 1e-6, "left of the line stays default");
        }
    }
    // Canonicalized: the session list is the exact derived piece, split at
    // the POSITION-APPLIED coordinate (x = 4, not the drawn ~3.8).
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1);
    assert!((session[0].delta_m - 0.3).abs() < 1e-6);
    let min_x = session[0]
        .polygon
        .iter()
        .map(|p| p.x)
        .fold(f32::INFINITY, f32::min);
    assert!(
        (min_x - 4.0).abs() < 1e-3,
        "position apply moved the line to exactly 4 (got {min_x})"
    );
}

/// The tap + H/V picker path is equivalent to the stroke path: a tapped point
/// plus "Vertical" yields the same line, side pick, position prompt, and
/// derived split.
#[test]
fn ceiling_step_tap_orientation_path_matches_the_stroke_path() {
    let mut c = ceiling_ready_controller();
    c.ceiling_step_tap(3.8, 5.0);
    assert!(c.ceiling_step_orientation_pending(), "tap → H/V choice");
    c.choose_ceiling_step_orientation(false); // vertical → position varies on X
    assert!(c.ceiling_step_side_pending());
    c.ceiling_step_tap(8.0, 5.0); // Greater side
    let ui = c.position_prompt_for_ui().expect("line position prompt");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    c.apply_position_dimension(4.0).unwrap();
    c.assign_ceiling_step_delta(0.3).unwrap();

    c.materialize();
    let zs = ceiling_flat_zs(&c);
    assert_eq!(zs.len(), 2);
    assert!((zs[0] - 2.4).abs() < 1e-6 && (zs[1] - 2.7).abs() < 1e-6);
    let session = &c.ceiling_step_regions[&0];
    let min_x = session[0]
        .polygon
        .iter()
        .map(|p| p.x)
        .fold(f32::INFINITY, f32::min);
    assert!((min_x - 4.0).abs() < 1e-3, "same split as the stroke path");
}

/// Skipping the position prompt keeps the DRAWN coordinate (the stroke's
/// fitted centroid, ≈ 3.8) instead of moving the line.
#[test]
fn ceiling_step_position_skip_keeps_the_drawn_line() {
    let mut c = ceiling_ready_controller();
    c.apply_ceiling_step_stroke_event(&[(3.78, 0.5), (3.82, 5.0), (3.8, 9.5)]);
    c.ceiling_step_tap(7.0, 5.0);
    assert!(c.position_prompt_for_ui().is_some());
    c.skip_position_dimension().unwrap();
    assert!(
        c.ceiling_step_amount_pending(),
        "skip → amount, drawn coord"
    );
    c.assign_ceiling_step_delta(-0.2).unwrap();

    c.materialize();
    let session = &c.ceiling_step_regions[&0];
    assert!((session[0].delta_m + 0.2).abs() < 1e-6, "lowered by 0.2");
    let min_x = session[0]
        .polygon
        .iter()
        .map(|p| p.x)
        .fold(f32::INFINITY, f32::min);
    assert!(
        (min_x - 3.8).abs() < 0.05,
        "drawn coordinate kept (got {min_x})"
    );
}

/// Reload reconstructs the session regions from the persisted ceilings, and a
/// no-edit re-derive keeps the split — the canonicalize-at-commit fixpoint.
#[test]
fn ceiling_step_split_survives_reload_and_rederive() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).unwrap();
    c.materialize();
    let before = ceiling_flat_zs(&c);
    assert_eq!(before.len(), 2);

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    // Session regions rebuilt from the persisted ceilings.
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "reconstructed from the stepped roof");
    assert!((session[0].delta_m - 0.3).abs() < 1e-6);

    // Re-derive without any edit: the split must be reproduced exactly
    // (derive → reconstruct → derive is a fixpoint).
    c.mark_dirty().unwrap();
    c.materialize();
    let after = ceiling_flat_zs(&c);
    assert_eq!(after.len(), 2, "split kept across the re-derive");
    assert!((after[0] - before[0]).abs() < 1e-6 && (after[1] - before[1]).abs() < 1e-6);
}

/// Deleting the step region re-flattens the roof to the default height on
/// the next derive.
#[test]
fn deleting_a_ceiling_step_reflattens_the_roof() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).unwrap();
    c.materialize();
    assert_eq!(ceiling_flat_zs(&c).len(), 2);

    delete_ceiling_step_region_at(&mut c, 0);
    c.materialize();
    let zs = ceiling_flat_zs(&c);
    assert!(
        zs.iter().all(|z| (z - 2.4).abs() < 1e-6),
        "roof back at the default everywhere (got {zs:?})"
    );
}

/// Delete the storey-0 ceiling-step region at raw-Vec index `i` by its world
/// ring — `delete_ceiling_step_region` takes a ring (the displayed feed is
/// divider-filtered, so display indices don't map to the raw Vec).
fn delete_ceiling_step_region_at(c: &mut AppController, i: usize) {
    let ring: Vec<(f64, f64)> = c.ceiling_step_regions[&0][i]
        .polygon
        .iter()
        .map(|p| (p.x as f64, p.y as f64))
        .collect();
    c.delete_ceiling_step_region(&ring);
}

/// A-T5: a same-height split (delta 0.0) — via `assign_ceiling_step_delta(0.0)`
/// and the `keep_ceiling_same_height` button alike — splits the active line
/// into two SAME-HEIGHT pieces, both kept in the session list, `mark_dirty`
/// set; the near-zero rejection no longer fires for 0.
#[test]
fn ceiling_same_height_split_keeps_both_pieces() {
    for via_button in [false, true] {
        let mut c = ceiling_ready_controller();
        ceiling_step_to_amount_at_x4(&mut c);
        if via_button {
            c.keep_ceiling_same_height()
                .expect("same-height split applies");
        } else {
            // The near-zero guard no longer rejects an exact 0.
            c.assign_ceiling_step_delta(0.0)
                .expect("zero delta is a divider, not a rejection");
        }
        assert!(!c.ceiling_step_pending(), "flow complete");
        assert!(c.is_dirty(), "the split marked the document dirty");

        c.materialize();
        // Two pieces, both at the default height (no riser).
        let zs = ceiling_flat_zs(&c);
        assert_eq!(zs.len(), 2, "split into two flat pieces");
        assert!(
            zs.iter().all(|z| (z - 2.4).abs() < 1e-6),
            "both pieces stay at the default height (got {zs:?})"
        );
        // The ONE divider drawn is the durable record (a delta-0 area); it still
        // splits the ceiling into the two flat roof pieces asserted above.
        let session = &c.ceiling_step_regions[&0];
        assert_eq!(session.len(), 1, "the drawn divider recorded once");
        assert!(
            session.iter().all(|r| r.delta_m.abs() < 1e-3),
            "it is a same-height divider"
        );
    }
}

/// A-T6: a same-height split survives reload + a no-edit re-derive — the
/// shell-level fixpoint (derive → reconstruct → derive reproduces the two
/// flat pieces).
#[test]
fn ceiling_same_height_split_survives_reload_and_rederive() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.0).unwrap();
    c.materialize();
    assert_eq!(ceiling_flat_zs(&c).len(), 2);

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(
        session.len(),
        1,
        "the divider reconstructed from the durable field"
    );
    assert!(session.iter().all(|r| r.delta_m.abs() < 1e-3));

    // Re-derive with no edit: the two flat pieces must persist (fixpoint).
    c.mark_dirty().unwrap();
    c.materialize();
    let zs = ceiling_flat_zs(&c);
    assert_eq!(zs.len(), 2, "split kept across the re-derive");
    assert!(zs.iter().all(|z| (z - 2.4).abs() < 1e-6));
}

/// A-T7: deleting the divider re-fuses the two halves to ONE flat default ring
/// on the next derive. The split records ONE delta-0 divider area (the durable
/// record), so removing the partition means clearing it — with no divider the
/// derive yields a single default ring.
#[test]
fn deleting_a_divider_refuses_the_halves() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.0).unwrap();
    c.materialize();
    assert_eq!(c.ceiling_step_regions[&0].len(), 1, "one divider");

    // Delete the dividers (the user removes the split). Each delete re-commits;
    // once none remain the next derive re-fuses to a single default ring.
    while c
        .ceiling_step_regions
        .get(&0)
        .is_some_and(|rs| !rs.is_empty())
    {
        delete_ceiling_step_region_at(&mut c, 0);
    }
    c.materialize();
    c.reconstruct_ceiling_step_regions_for(0);
    let zs = ceiling_flat_zs(&c);
    assert_eq!(zs.len(), 1, "the two halves re-fused into one flat region");
    assert!((zs[0] - 2.4).abs() < 1e-6);
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "no session divider survives a lone flat-default region"
    );
}

/// A-T8: a divider's label reads "divider"; the amber feed
/// (`ceiling_step_region_polygons`) EXCLUDES it; `ceiling_divider_lines`
/// emits its cut segment.
#[test]
fn divider_label_feed_and_cut_segment() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.0).unwrap();
    c.materialize();
    c.reconstruct_ceiling_step_regions_for(0);

    // Label.
    assert_eq!(AppController::ceiling_step_region_label(0.0), "divider");
    assert_eq!(AppController::ceiling_step_region_label(0.2), "+0.20 m");

    // The amber tint/label feed excludes dividers — both halves are dividers.
    assert!(
        c.ceiling_step_region_polygons().is_empty(),
        "dividers don't show as amber height steps"
    );

    // The faint dashed cut: one segment per divider, on the split axis (X = 4).
    let cuts = c.ceiling_divider_lines();
    assert!(!cuts.is_empty(), "divider cut segments emitted");
    for ((ax, _ay), (bx, _by)) in &cuts {
        assert!(
            (ax - 4.0).abs() < 1e-3 && (bx - 4.0).abs() < 1e-3,
            "the cut is the shared edge at x = 4 (got {ax}..{bx})"
        );
    }
}

/// A-T8b (delete contract): with a divider PRECEDING a height step in the raw
/// region Vec, the displayed (divider-filtered) feed lists only the step;
/// deleting via that feed's ring removes the STEP, not the divider — the
/// world-ring lookup maps the filtered entry back to its raw Vec index.
#[test]
fn deleting_from_the_filtered_feed_removes_the_right_region() {
    let mut c = ceiling_ready_controller();
    // Raw Vec order: [divider(Δ0) at x = 4 Greater, step(+0.3) at x = 7 Greater].
    let divider = c
        .document
        .ceiling_step_region(ParameterAxis::X, 4.0, document::LineSide::Greater, 0.0)
        .expect("divider builds");
    let step = c
        .document
        .ceiling_step_region(ParameterAxis::X, 7.0, document::LineSide::Greater, 0.3)
        .expect("step builds");
    c.ceiling_step_regions.entry(0).or_default().push(divider);
    c.ceiling_step_regions.entry(0).or_default().push(step);

    // The displayed feed is filtered to the height step alone.
    let feed = c.ceiling_step_region_polygons();
    assert_eq!(
        feed.len(),
        1,
        "only the +0.30 step shows; the divider is hidden"
    );
    assert_eq!(feed[0].1, "+0.30 m");

    // Delete via the FEED's ring (what the UI button carries). It must remove
    // the step (raw index 1), not the divider (raw index 0).
    c.delete_ceiling_step_region(&feed[0].0);
    let remaining = &c.ceiling_step_regions[&0];
    assert_eq!(remaining.len(), 1, "one region left");
    assert!(
        remaining[0].delta_m.abs() < 1e-3,
        "the surviving region is the DIVIDER, not the step (got delta {})",
        remaining[0].delta_m
    );
}

/// A storey switch abandons the in-flight line and leaves the mode — its
/// coordinate is in the outgoing storey's space.
#[test]
fn storey_switch_clears_pending_ceiling_step() {
    let mut c = ceiling_ready_controller();
    c.apply_ceiling_step_stroke_event(&[(3.78, 0.5), (3.82, 5.0), (3.8, 9.5)]);
    assert!(c.ceiling_step_side_pending());
    c.add_storey().unwrap();
    assert!(!c.ceiling_step_mode(), "mode reset on switch");
    assert!(!c.ceiling_step_pending(), "pending line abandoned");
}

/// The assign validation: a non-finite step and a drop at or past the default
/// height are rejected, with the pending line left intact so the user can
/// retype; a valid amount still lands afterwards. (A near-zero |Δ| < 0.01 is
/// NO LONGER rejected — it snaps to 0.0, a same-height divider; see
/// `ceiling_same_height_split_keeps_both_pieces`.)
#[test]
fn ceiling_step_amount_rejections_keep_the_pending_line() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);

    assert!(
        c.assign_ceiling_step_delta(f32::NAN).is_err(),
        "non-finite rejected"
    );
    assert!(c.ceiling_step_amount_pending(), "pending intact");
    assert!(
        c.assign_ceiling_step_delta(-2.4).is_err(),
        "Δ ≤ −default rejected (ceiling would reach the floor)"
    );
    assert!(c.ceiling_step_amount_pending(), "pending intact");
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "nothing committed by the rejections"
    );

    c.assign_ceiling_step_delta(0.2)
        .expect("valid amount lands");
    assert!(!c.ceiling_step_pending());
    c.materialize();
    assert_eq!(ceiling_flat_zs(&c).len(), 2);
}

// ─── Ceiling steps (carve — closed stroke, floor-step parity) ───────────────

/// Unsigned shoelace area of a session-region ring.
fn session_ring_area(ring: &[Point2f32]) -> f64 {
    let n = ring.len();
    let mut s = 0.0_f64;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        s += a.x as f64 * b.y as f64 - b.x as f64 * a.y as f64;
    }
    s.abs() / 2.0
}

/// Draw the interior 4×4 carve at (2,2)–(6,6) in ceiling-step mode and drive
/// it to the amount stage (lengths 4 m, drawn position accepted).
fn ceiling_carve_to_amount(c: &mut AppController) {
    c.apply_ceiling_step_stroke_event(&[
        (2.0, 2.0),
        (6.0, 2.0),
        (6.0, 6.0),
        (2.0, 6.0),
        (2.1, 2.1),
    ]);
    complete_interior_length_loop(c);
    skip_position_prompts(c);
    assert!(c.ceiling_step_amount_pending(), "amount stage open");
}

/// THE CARVE FLOW END-TO-END: a CLOSED stroke in ceiling-step mode carves an
/// area (not a line), dimensions it as the `StepArea` shape — wall lengths,
/// then the position prompts — with the FLOOR amount UI never showing, then
/// opens the SAME delta stage as the line flow; assigning the delta stores
/// the region, splits the roof at the area, and returns to the outline.
#[test]
fn ceiling_carve_closed_stroke_dimensions_then_takes_the_delta() {
    let mut c = ceiling_ready_controller();
    c.apply_ceiling_step_stroke_event(&[
        (2.0, 2.0),
        (6.0, 2.0),
        (6.0, 6.0),
        (2.0, 6.0),
        (2.1, 2.1),
    ]);
    assert!(
        !c.ceiling_step_side_pending(),
        "a closed stroke is an area, not a line"
    );
    assert!(c.ceiling_step_pending(), "the carve is in flight");
    assert!(
        c.dimensioning_active(),
        "the drawn walls prompt for lengths"
    );
    assert!(
        !c.step_pick_pending(),
        "the FLOOR amount UI never shows for a ceiling carve"
    );
    assert!(
        !c.ceiling_step_amount_pending(),
        "delta gated by the length loop"
    );

    complete_interior_length_loop(&mut c);
    assert!(
        c.document.dimensioning.current_position_prompt.is_some(),
        "an interior area's position prompts follow the lengths"
    );
    assert!(!c.ceiling_step_amount_pending(), "delta gated by position");
    skip_position_prompts(&mut c);
    assert!(
        c.ceiling_step_amount_pending(),
        "the line flow's amount stage opens"
    );
    assert!(!c.step_pick_pending(), "still not a floor pick");
    assert!(
        c.pending_step_polygon().len() >= 4,
        "the dimensioned ring highlights like a floor step"
    );
    assert!(
        c.pending_ceiling_step_line().is_none(),
        "an area has no dashed-line overlay"
    );

    c.assign_ceiling_step_delta(0.3).expect("delta applies");
    assert!(!c.ceiling_step_pending(), "amount consumed");
    assert_eq!(
        c.document.active,
        ActiveShape::Outline,
        "back to the outline"
    );
    assert_eq!(c.document.floor_plan.get_points().len(), 4);
    // The assign canonicalized: the session list is the derived piece — the
    // carved 4×4 at +0.3.
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "one canonical step region");
    assert!((session[0].delta_m - 0.3).abs() < 1e-6);
    assert!(
        (session_ring_area(&session[0].polygon) - 16.0).abs() < 0.1,
        "the carved 4×4 area: {:?}",
        session[0].polygon
    );
    // The roof split: 2.7 over the carve, 2.4 elsewhere.
    let zs = ceiling_flat_zs(&c);
    assert!(zs.iter().any(|z| (z - 2.7).abs() < 1e-6), "{zs:?}");
    assert!(zs.iter().any(|z| (z - 2.4).abs() < 1e-6), "{zs:?}");
}

/// An OPEN cut with three drawn sides whose ends meet the outline — NOT a
/// closed loop and NOT a single straight line — must be accepted through the
/// SAME cut pipeline the floor step uses (a two-region side-pick), not rejected
/// as "not a straight line". Regression: this stroke previously fell into the
/// step-LINE fit and was dropped; the fix reuses `pending_step_cut` /
/// `choose_step_cut_region` (no ceiling-specific cut UI).
#[test]
fn ceiling_open_three_sided_cut_reuses_the_step_cut_pick() {
    let mut c = ceiling_ready_controller();
    // Three drawn sides whose ends meet two different outline walls (left x=0 &
    // right x=10): a cornered cut that splits the ceiling in two.
    c.apply_ceiling_step_stroke_event(&[(0.0, 7.0), (6.0, 7.0), (6.0, 2.0), (10.0, 2.0)]);
    assert!(
        !c.ceiling_step_side_pending(),
        "a cornered cut is not a straight step line"
    );
    let regions = c.pending_step_cut_regions();
    assert_eq!(
        regions.len(),
        2,
        "the cut splits the ceiling into two regions to pick from (reused floor-step pick)"
    );
    // Pick the lower region → it dimensions as the StepArea; under ceiling-step
    // mode it awaits a DELTA afterward, so the floor level UI never shows.
    c.choose_step_cut_region(5.0, 1.0);
    assert!(
        c.pending_step_cut_regions().is_empty(),
        "the tap consumed the cut pick"
    );
    assert!(
        c.dimensioning_active(),
        "the chosen region dimensions its drawn walls"
    );
    assert!(
        !c.step_pick_pending(),
        "still a ceiling step — the floor level UI never shows"
    );
}

/// Carve-flow guards, floor-step parity: the delta validations reject without
/// dropping the pending area; Cancel abandons it back to the outline; undo
/// after the carve restores the outline (the carve pushed a history snapshot,
/// and the pending state is the document — nothing dangles).
#[test]
fn ceiling_carve_rejections_cancel_and_undo_mirror_the_floor_flow() {
    let mut c = ceiling_ready_controller();
    ceiling_carve_to_amount(&mut c);

    assert!(
        c.assign_ceiling_step_delta(f32::INFINITY).is_err(),
        "non-finite rejected"
    );
    assert!(c.ceiling_step_amount_pending(), "area stays pending");
    assert!(
        c.assign_ceiling_step_delta(-2.4).is_err(),
        "Δ ≤ −default rejected"
    );
    assert!(c.ceiling_step_amount_pending(), "area stays pending");
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "nothing committed by the rejections"
    );

    c.cancel_pending_ceiling_step();
    assert!(!c.ceiling_step_pending(), "cancel clears the carve");
    assert_eq!(c.document.active, ActiveShape::Outline);
    assert_eq!(c.document.floor_plan.get_points().len(), 4);

    // Undo right after a fresh carve stroke (each dimension answer pushes its
    // own snapshot, so undo there rewinds one answer — the floor parity):
    // back to the outline, nothing pending.
    c.apply_ceiling_step_stroke_event(&[
        (2.0, 2.0),
        (6.0, 2.0),
        (6.0, 6.0),
        (2.0, 6.0),
        (2.1, 2.1),
    ]);
    assert!(c.ceiling_step_pending(), "carve in flight");
    c.undo().expect("undo applies");
    assert_eq!(c.document.active, ActiveShape::Outline, "carve rewound");
    assert!(!c.ceiling_step_pending(), "no dangling pending state");
    assert!(c.ceiling_step_mode(), "mode itself stays on");
}

/// THE CARVE RELOAD FIXPOINT: carve → delta → resolve, then reload from the
/// DB — the session regions reconstruct to the SAME canonical piece (ring
/// area + delta) — and a re-derive reproduces the identical roof split
/// (derive → reconstruct → derive is a fixpoint), reconstruction stable.
#[test]
fn ceiling_carve_split_survives_reload_and_rederive() {
    let mut c = ceiling_ready_controller();
    ceiling_carve_to_amount(&mut c);
    c.assign_ceiling_step_delta(0.3).expect("delta applies");
    c.materialize();
    let before_zs = ceiling_flat_zs(&c);
    let before_area = session_ring_area(&c.ceiling_step_regions[&0][0].polygon);
    assert!((before_area - 16.0).abs() < 0.1);

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    // Session regions rebuilt from the persisted ceilings — the same piece.
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "reconstructed from the stepped roof");
    assert!((session[0].delta_m - 0.3).abs() < 1e-6);
    assert!(
        (session_ring_area(&session[0].polygon) - before_area).abs() < 1e-3,
        "reconstruction matches the carved area"
    );

    // Re-derive without any edit: the split is reproduced exactly.
    c.mark_dirty().unwrap();
    c.materialize();
    let after_zs = ceiling_flat_zs(&c);
    assert_eq!(before_zs.len(), after_zs.len(), "split kept");
    for (b, a) in before_zs.iter().zip(&after_zs) {
        assert!((b - a).abs() < 1e-6, "{before_zs:?} vs {after_zs:?}");
    }
    // And the reconstruction itself is stable across the round trip.
    c.reconstruct_ceiling_step_regions_for(0);
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1);
    assert!((session_ring_area(&session[0].polygon) - before_area).abs() < 1e-3);
}

/// A phase-tagged storey's CEILING exposure survives a Layer-1-only reload. The
/// load-path `heal` recomposes a phase-tagged storey's ceilings via
/// `default_for_footprint` (DEFAULT exposure), so the seed MUST capture the saved
/// exposures BEFORE heal runs — otherwise it reads the wiped ceilings and the
/// ceiling reverts to `Unset` (red). The snapshot no longer carries
/// `ceiling_exposure` (it's `skip_serializing`), so this bites the DB path too,
/// not just the checkpoint.
#[test]
fn phase_tagged_ceiling_exposure_survives_reload() {
    use ber_model::ber::structural::CeilingExposure;
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = ceiling_ready_controller();
    c.set_ceiling_step_mode(false);
    // Phase-tag the right half so the reload heal recomposes this storey's ceilings.
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
        phase: ConstructionPhase::Extension1,
    });
    c.mark_dirty().unwrap();
    c.materialize();
    // Label every roof region's ceiling exposure.
    let keys: Vec<_> = c.current_record.as_ref().unwrap().data.storeys[0]
        .ceilings
        .regions
        .iter()
        .map(|r| document::centroid_key(r.centroid()))
        .collect();
    for k in keys {
        c.document.storeys[0]
            .ceiling_exposure
            .insert(k, CeilingExposure::ExposedRoof);
    }
    c.commit_to_ber().unwrap();
    c.materialize();
    assert!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .ceilings
            .regions
            .iter()
            .all(|r| r.exposure == CeilingExposure::ExposedRoof),
        "sanity: all ceilings labelled before save"
    );

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();

    let ceilings = &c.current_record.as_ref().unwrap().data.storeys[0]
        .ceilings
        .regions;
    assert!(!ceilings.is_empty(), "ceilings exist after reload");
    assert!(
        ceilings
            .iter()
            .all(|r| r.exposure == CeilingExposure::ExposedRoof),
        "ceiling exposure survived a phase-tagged reload (seed runs before heal)"
    );
}

/// MIGRATION SELF-HEAL: a record saved BEFORE the per-phase-zone fix baked a
/// spurious phase-induced split into its stored ceilings (a uniform-phase roof
/// fragmented by a redundant coplanar same-construction boundary). Ceilings are
/// regions-as-truth — resolve keeps them verbatim and the phase carve only
/// SPLITS — so the stale split never self-heals. `open_ber` must rebuild each
/// phase-tagged storey's ceilings from its durable inputs and re-apply the clean
/// per-phase carve, dropping the stale split while PRESERVING a deliberate
/// structure roof-split and a ceiling step.
#[test]
fn open_ber_heals_spurious_phase_induced_roof_split() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let p64 = |x: f64, y: f64| Point2f64::new(x, y);

    let mut c = ceiling_ready_controller();
    c.set_ceiling_step_mode(false);

    // Tag the right half (x>=5) as Extension1 — a genuine phase boundary at x=5.
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
        phase: ConstructionPhase::Extension1,
    });

    // A DELIBERATE structure roof-split at y=5 (durable `ceiling_splits`) — must
    // survive the heal. A ceiling step in the corner — must survive too.
    let split_ring = vec![p64(0., 0.), p64(10., 0.), p64(10., 5.), p64(0., 5.)];
    c.current_record.as_mut().unwrap().data.storeys[0]
        .ceiling_splits
        .push(split_ring.clone());
    c.ceiling_step_regions.insert(
        0,
        vec![CeilingStepRegion {
            polygon: vec![p(1., 1.), p(3., 1.), p(3., 3.), p(1., 3.)],
            delta_m: 0.4,
        }],
    );
    c.reconstruct_ceiling_step_regions_for(0); // mirror to durable `ceiling_steps`
    c.mark_dirty().unwrap();
    c.recompose_ceilings(0); // compose step + split into the ceilings (regions-as-truth)
    c.materialize(); // re-resolve applies the clean per-phase carve

    let clean_count = c.current_record.as_ref().unwrap().data.storeys[0]
        .ceilings
        .regions
        .len();

    // SIMULATE THE PRE-FIX BAKE: inject an EXTRA redundant coplanar split at
    // x=7 — a same-construction, same-z boundary inside the (already phase- and
    // split-carved) Extension1 right half. This is the stale fragmentation the
    // old per-floor-region phase zoning produced; it is in NO durable field.
    let spurious_ring = vec![p64(7., 0.), p64(10., 0.), p64(10., 10.), p64(7., 10.)];
    {
        let storey = &mut c.current_record.as_mut().unwrap().data.storeys[0];
        storey.ceilings =
            std::mem::take(&mut storey.ceilings).split_faces_coplanar(&[spurious_ring]);
    }
    let spurious_count = c.current_record.as_ref().unwrap().data.storeys[0]
        .ceilings
        .regions
        .len();
    assert!(
        spurious_count > clean_count,
        "spurious split added regions: {spurious_count} vs clean {clean_count}"
    );

    // Persist the spurious state DIRECTLY (bypassing the re-materialize in
    // flush_if_dirty, which would re-derive and wipe the injected split).
    let id = c.current_record.as_ref().unwrap().id.clone();
    c.persist().unwrap();

    // RELOAD: the heal must recompose from durable inputs and re-carve cleanly.
    c.open_ber(&id).unwrap();
    let healed = &c.current_record.as_ref().unwrap().data.storeys[0]
        .ceilings
        .regions;
    assert_eq!(
        healed.len(),
        clean_count,
        "stale phase-induced split dropped: healed {} == clean {clean_count}",
        healed.len()
    );

    // The deliberate structure roof-split SURVIVED (durable `ceiling_splits` intact
    // and replayed): there's a face boundary at y=5, i.e. faces on both sides.
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(
        storey.ceiling_splits.len(),
        1,
        "deliberate structure roof-split preserved durably"
    );
    let has_below = storey
        .ceilings
        .regions
        .iter()
        .any(|r| r.polygon.exterior.iter().all(|v| v.y <= 5.0 + 1e-6));
    let has_above = storey
        .ceilings
        .regions
        .iter()
        .any(|r| r.polygon.exterior.iter().all(|v| v.y >= 5.0 - 1e-6));
    assert!(
        has_below && has_above,
        "deliberate y=5 split still partitions the roof"
    );

    // The ceiling step SURVIVED — reconstructed from the durable `ceiling_steps`.
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "ceiling step reconstructed on reload");
    assert!(
        (session[0].delta_m - 0.4).abs() < 1e-6,
        "step delta preserved"
    );
}

/// An UNTAGGED (no-phase) storey's ceilings are byte-stable across a reload — the
/// heal is gated on phase tags, so `recompose_ceilings` never touches it and the
/// regions-as-truth reload fixpoint is preserved exactly.
#[test]
fn open_ber_leaves_untagged_roofs_byte_stable() {
    let mut c = ceiling_ready_controller();
    c.set_ceiling_step_mode(false);
    // A deliberate ceiling step, but NO phase tag.
    c.ceiling_step_regions.insert(
        0,
        vec![CeilingStepRegion {
            polygon: vec![
                Point2f32::new(2., 2.),
                Point2f32::new(6., 2.),
                Point2f32::new(6., 6.),
                Point2f32::new(2., 6.),
            ],
            delta_m: 0.5,
        }],
    );
    c.reconstruct_ceiling_step_regions_for(0);
    c.mark_dirty().unwrap();
    c.recompose_ceilings(0);
    c.materialize();

    let fingerprint = |c: &AppController| {
        c.current_record.as_ref().unwrap().data.storeys[0]
            .ceilings
            .regions
            .iter()
            .map(|r| format!("{:?}", r.polygon))
            .collect::<Vec<_>>()
            .join("|")
    };
    let before = fingerprint(&c);

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    assert!(
        c.phase_regions.get(&0).is_none_or(|v| v.is_empty()),
        "storey is untagged"
    );
    assert_eq!(
        fingerprint(&c),
        before,
        "untagged ceilings are byte-identical across reload (no heal perturbation)"
    );
}

/// A storey switch abandons a half-dimensioned ceiling CARVE with the mode —
/// it must not resurface as a FLOOR-step pick when switching back.
#[test]
fn storey_switch_abandons_an_in_flight_ceiling_carve() {
    let mut c = ceiling_ready_controller();
    c.apply_ceiling_step_stroke_event(&[
        (2.0, 2.0),
        (6.0, 2.0),
        (6.0, 6.0),
        (2.0, 6.0),
        (2.1, 2.1),
    ]);
    assert!(c.ceiling_step_pending(), "carve in flight");
    c.add_storey().unwrap();
    assert!(!c.ceiling_step_mode(), "mode reset on switch");
    assert!(!c.ceiling_step_pending(), "carve abandoned");
    c.set_current_storey(Some(0)).unwrap();
    assert_eq!(
        c.document.active,
        ActiveShape::Outline,
        "no StepArea shape lingers on the original storey"
    );
    assert!(!c.step_pick_pending(), "…so no floor-step pick resurfaces");
}

// ─── Ceiling-step line snapping ─────────────────────────────────────────────

/// An L-shaped storey (10×10 with the top-right 4×6 bite removed), default
/// ceiling height set and ceiling-step mode entered. Its vertical wall at
/// x = 6 and horizontal wall at y = 4 are the only NON-extreme parallel
/// references — the snap targets (the bbox-extreme boundary walls never
/// snap; a 10×10 square has no target at all).
fn ceiling_ready_l_controller() -> AppController {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    c.document.floor_plan = FloorPlan::from_clean_ring(&[
        p(0., 0.),
        p(10., 0.),
        p(10., 4.),
        p(6., 4.),
        p(6., 10.),
        p(0., 10.),
    ])
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
    c.set_default_ceiling_height(2.4).expect("height applies");
    c.set_ceiling_step_mode(true);
    assert!(c.ceiling_step_mode());
    c
}

/// A stroke within snap tolerance of a parallel wall SNAPS onto it: the
/// line's coordinate becomes the wall's exactly, the snap tag renders (empty
/// options + skippable — the menu offers only "Don't snap"), the side pick
/// SKIPS the position prompt (the coordinate is exact by construction), and
/// the assigned step splits the roof at the wall's coordinate.
#[test]
fn ceiling_step_line_within_tolerance_snaps_to_the_wall() {
    let mut c = ceiling_ready_l_controller();
    // Drawn at x ≈ 5.9 — within 0.2 of the interior wall at x = 6.
    c.apply_ceiling_step_stroke_event(&[(5.88, 0.5), (5.92, 4.0), (5.9, 9.5)]);
    assert!(c.ceiling_step_side_pending());
    let ((lx, _), _) = c.pending_ceiling_step_line().unwrap();
    assert!((lx - 6.0).abs() < 1e-6, "coordinate snapped to the wall");

    let tag = c.ceiling_step_snap_tag().expect("snap tag present");
    assert!((tag.pos.0 - 6.0).abs() < 1e-4, "tag on the snapped line");
    assert!(
        (tag.from.0 - 5.9).abs() < 0.05,
        "identity = the drawn point"
    );
    assert_eq!(tag.current_kind, Some(manip_plan::SnapKind::ToParallelWall));
    assert!(
        tag.options.is_empty() && tag.skippable,
        "menu = Don't snap only"
    );

    c.ceiling_step_tap(2.0, 5.0); // left of the line → Less side
    assert!(
        c.position_prompt_for_ui().is_none(),
        "snapped line skips the position prompt"
    );
    assert!(c.ceiling_step_amount_pending(), "straight to the amount");
    assert!(c.ceiling_step_snap_tag().is_some(), "tag still clickable");

    c.assign_ceiling_step_delta(0.3).expect("amount applies");
    c.materialize();
    // The stepped (Less) session piece ends exactly at the wall, x = 6.
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1);
    let max_x = session[0]
        .polygon
        .iter()
        .map(|p| p.x)
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(
        (max_x - 6.0).abs() < 1e-3,
        "assign landed at the wall's coordinate (got {max_x})"
    );
}

/// Beyond the snap tolerance nothing changes: no snap, no tag, and the side
/// pick opens the position prompt as normal with the drawn coordinate.
#[test]
fn ceiling_step_line_beyond_tolerance_does_not_snap() {
    let mut c = ceiling_ready_l_controller();
    // Drawn at x ≈ 5.0 — 1.0 m from the wall at x = 6.
    c.apply_ceiling_step_stroke_event(&[(4.98, 0.5), (5.02, 4.0), (5.0, 9.5)]);
    assert!(c.ceiling_step_side_pending());
    let ((lx, _), _) = c.pending_ceiling_step_line().unwrap();
    assert!((lx - 5.0).abs() < 0.05, "drawn coordinate kept");
    assert!(c.ceiling_step_snap_tag().is_none(), "no tag without a snap");

    c.ceiling_step_tap(2.0, 5.0);
    let ui = c
        .position_prompt_for_ui()
        .expect("position prompt as normal");
    assert_eq!(ui.kind, PositionPromptKind::Line);
}

/// Un-snapping from the AMOUNT stage reverts the line to its DRAWN coordinate
/// and reopens the skipped position question; applying it then moves the line
/// like any un-snapped flow.
#[test]
fn unsnap_from_amount_reopens_the_position_prompt_with_the_drawn_coord() {
    let mut c = ceiling_ready_l_controller();
    c.apply_ceiling_step_stroke_event(&[(5.88, 0.5), (5.92, 4.0), (5.9, 9.5)]);
    c.ceiling_step_tap(2.0, 5.0); // snapped → Amount, no position prompt
    assert!(c.ceiling_step_amount_pending());

    c.unsnap_ceiling_step_line();
    let ui = c
        .position_prompt_for_ui()
        .expect("position prompt reopened");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    let ((lx, _), _) = c.pending_ceiling_step_line().unwrap();
    assert!((lx - 5.9).abs() < 0.05, "line back at the DRAWN coordinate");
    assert!(c.ceiling_step_snap_tag().is_none(), "snap dropped");

    // The reopened prompt works as normal: skip keeps the drawn coordinate.
    c.skip_position_dimension().unwrap();
    assert!(c.ceiling_step_amount_pending());
    c.assign_ceiling_step_delta(0.3).unwrap();
    c.materialize();
    let session = &c.ceiling_step_regions[&0];
    let max_x = session[0]
        .polygon
        .iter()
        .map(|p| p.x)
        .fold(f32::NEG_INFINITY, f32::max);
    assert!((max_x - 5.9).abs() < 0.05, "split at the drawn coordinate");
}

/// Un-snapping from the SIDE stage reverts the coordinate but stays at the
/// side pick; the position prompt then follows the side tap as normal.
#[test]
fn unsnap_from_side_stays_at_the_side_pick() {
    let mut c = ceiling_ready_l_controller();
    c.apply_ceiling_step_stroke_event(&[(5.88, 0.5), (5.92, 4.0), (5.9, 9.5)]);
    assert!(c.ceiling_step_snap_tag().is_some());

    c.unsnap_ceiling_step_line();
    assert!(c.ceiling_step_side_pending(), "still awaiting the side tap");
    assert!(c.ceiling_step_snap_tag().is_none());
    let ((lx, _), _) = c.pending_ceiling_step_line().unwrap();
    assert!((lx - 5.9).abs() < 0.05, "drawn coordinate restored");

    c.ceiling_step_tap(2.0, 5.0);
    assert!(
        c.position_prompt_for_ui().is_some(),
        "un-snapped side pick opens the position prompt as normal"
    );
}

/// Walls at the outline bbox EXTREMES never snap — a line on the boundary
/// would leave one side empty. Drawn 0.1 m inside the square's left wall,
/// the line keeps its drawn coordinate and asks the position question.
#[test]
fn bbox_extreme_walls_never_snap() {
    let mut c = ceiling_ready_controller(); // 10×10 square: all walls extreme
    c.apply_ceiling_step_stroke_event(&[(0.1, 0.5), (0.1, 5.0), (0.1, 9.5)]);
    assert!(c.ceiling_step_side_pending());
    assert!(
        c.ceiling_step_snap_tag().is_none(),
        "no snap at the extreme"
    );
    let ((lx, _), _) = c.pending_ceiling_step_line().unwrap();
    assert!((lx - 0.1).abs() < 0.05, "drawn coordinate kept");
    c.ceiling_step_tap(5.0, 5.0);
    assert!(c.position_prompt_for_ui().is_some(), "prompt not skipped");
}

/// The tap + H/V path snaps identically to the stroke path: same snapped
/// coordinate, same tag, same position-prompt skip.
#[test]
fn ceiling_step_tap_orientation_path_snaps_identically() {
    let mut c = ceiling_ready_l_controller();
    c.ceiling_step_tap(5.9, 5.0);
    assert!(c.ceiling_step_orientation_pending());
    c.choose_ceiling_step_orientation(false); // vertical → position varies on X
    assert!(c.ceiling_step_side_pending());
    let ((lx, _), _) = c.pending_ceiling_step_line().unwrap();
    assert!((lx - 6.0).abs() < 1e-6, "tapped point snapped to the wall");
    assert!(c.ceiling_step_snap_tag().is_some());

    c.ceiling_step_tap(8.0, 2.0); // right of the line → Greater side
    assert!(c.position_prompt_for_ui().is_none(), "prompt skipped");
    assert!(c.ceiling_step_amount_pending());
}

// ─── Roof points (apex tap + per-axis position + height) ───────────────────

/// A finalised 10×10 storey with its default ceiling height set (2.4 m) and
/// roof-point mode entered — the starting point of every apex flow.
fn ceiling_apex_ready_controller() -> AppController {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).expect("height applies");
    c.set_roof_point_mode(true);
    assert!(c.ceiling_point_mode(), "mode enters once available");
    c
}

/// The mode toggle is gated exactly like the ceiling steps: refused while the
/// storey has no default ceiling height, available once it's set, and never
/// available on the read-only All tab. A tap outside the footprint is
/// rejected (nothing goes pending).
#[test]
fn ceiling_point_mode_gated_like_ceiling_steps() {
    let mut c = dimensioned_square_controller();
    assert!(!c.ceiling_point_available(), "no default height yet");
    c.set_roof_point_mode(true);
    assert!(!c.ceiling_point_mode(), "setter re-enforces the gate");

    c.set_default_ceiling_height(2.4).unwrap();
    assert!(c.ceiling_point_available());
    c.set_roof_point_mode(true);
    assert!(c.ceiling_point_mode());

    // A tap outside the footprint bbox is rejected up front.
    c.ceiling_point_tap(15.0, 5.0);
    assert!(!c.ceiling_point_pending(), "off-footprint tap rejected");

    // The All tab is read-only — not available there (the switch also resets
    // the mode like the other area tools).
    c.set_current_storey(None).unwrap();
    assert!(!c.ceiling_point_available());
    assert!(!c.ceiling_point_mode());
}

/// The full un-snapped flow: tap → X position prompt (kind `Point`, the
/// "area wall" degenerate at the tapped point) → Y prompt → height → assign
/// FANS the containing ceiling region: one triangle per boundary edge, every
/// triangle holding the apex vertex at the entered height, boundary corners
/// at the base. The applied positions land exactly; the session ceiling-step
/// list stays EMPTY (a fan over the default base reconstructs to nothing —
/// the apex is durable input, not session state).
#[test]
fn ceiling_point_tap_x_y_height_flow_fans_the_region() {
    let mut c = ceiling_apex_ready_controller();
    c.ceiling_point_tap(3.8, 5.2);
    let ui = c.position_prompt_for_ui().expect("X position prompt");
    assert_eq!(ui.kind, PositionPromptKind::Point);
    assert_eq!(ui.axis, ParameterAxis::X);
    assert_eq!(
        ui.area_wall.wall_a, ui.area_wall.wall_b,
        "a point, not a wall"
    );
    assert!((ui.area_wall.wall_a.0 - 3.8).abs() < 1e-4);
    // Nearest ref is x = 0 (3.8 m): 4 m from it → x = 4.
    c.apply_position_dimension(4.0).expect("X applies");

    let ui = c.position_prompt_for_ui().expect("Y position prompt");
    assert_eq!(ui.kind, PositionPromptKind::Point);
    assert_eq!(ui.axis, ParameterAxis::Y);
    // Nearest ref is y = 10 (4.8 m), drawn side −: 5 m from it → y = 5.
    c.apply_position_dimension(5.0).expect("Y applies");
    assert!(
        c.ceiling_point_height_pending(),
        "both axes pinned → height"
    );
    assert_eq!(
        c.pending_roof_point_position(),
        Some((4.0, 5.0)),
        "applied positions land exactly"
    );

    c.assign_roof_apex(4.2).expect("apex assigns");
    assert!(!c.ceiling_point_pending(), "flow complete");
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(
        storey.ceilings.regions.len(),
        4,
        "one fan triangle per boundary edge"
    );
    for region in &storey.ceilings.regions {
        assert_eq!(region.polygon.exterior.len(), 3, "triangles");
        assert!(
            region
                .polygon
                .exterior
                .iter()
                .any(|v| (v.x - 4.0).abs() < 1e-6
                    && (v.y - 5.0).abs() < 1e-6
                    && (v.z - 4.2).abs() < 1e-6),
            "every triangle holds the apex at the entered height"
        );
        assert!(
            region
                .polygon
                .exterior
                .iter()
                .all(|v| (v.z - 4.2).abs() < 1e-6 || (v.z - 2.4).abs() < 1e-6),
            "boundary corners stay at the base"
        );
    }
    assert_eq!(storey.ceiling_apexes.len(), 1, "the durable input");
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "a fan over the default base needs NO session step state"
    );
    assert_eq!(
        c.ceiling_apex_views(),
        vec![((4.0, 5.0), "▲ 4.20 m".into())]
    );
}

/// Entering a plan-takeover Mapping tool (roof / floor-exposure /
/// ceiling-exposure) from the 3D view drops the canvas back to the 2D plan.
/// Their takeovers replace the canvas with a 2D plan but their tap input only
/// routes in `SketchMode::Storey2D` — without the reset the assessor saw the
/// areas with DEAD input (the "can't click the roof areas after viewing 3D"
/// bug). The tap assertion proves the input path is live again end-to-end.
#[test]
fn entering_plan_takeover_tools_from_3d_returns_to_2d() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();

    c.set_sketch_mode(SketchMode::Storey3D);
    assert_eq!(c.sketch_mode(), SketchMode::Storey3D);
    c.set_roof_mode(true);
    assert_eq!(
        c.sketch_mode(),
        SketchMode::Storey2D,
        "the roof step is a plan task — entering it leaves 3D"
    );
    c.confirm_all_storeys_added_roof();
    c.roof_region_tap(5.0, 5.0, 0.5);
    assert!(
        c.roof_area_can_pick_shape(),
        "the tap lands — grouping works straight after coming from 3D"
    );
    c.set_roof_mode(false);

    c.set_sketch_mode(SketchMode::Storey3D);
    c.set_floor_exposure_mode(true);
    assert_eq!(c.sketch_mode(), SketchMode::Storey2D, "floor exposure too");
    c.set_floor_exposure_mode(false);

    c.set_sketch_mode(SketchMode::Storey3D);
    c.set_ceiling_exposure_mode(true);
    assert_eq!(
        c.sketch_mode(),
        SketchMode::Storey2D,
        "ceiling exposure too"
    );
}

/// Phase-1 roof step: group the ceiling area, pick GABLE, draw a ridge, type a
/// pitch. The roof lands on `Ber.roof` as a SEPARATE structure — the ceiling
/// (`storey.ceiling_apexes`) is left UNTOUCHED (the bug this replaced reshaped the
/// ceiling). A gable extends the ridge to the area's full span (x = 5, y 0..10).
#[test]
fn roof_area_gable_commits_to_ber_roof_without_touching_ceiling() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.set_roof_mode(true);
    assert!(c.roof_mode());
    c.confirm_all_storeys_added_roof();
    assert!(!c.roof_gate_pending());

    c.roof_region_tap(5.0, 5.0, 0.5);
    assert!(c.roof_area_can_pick_shape(), "the area is grouped");

    c.pick_roof_area_shape(RoofShape::Gable);
    assert!(c.roof_area_ridge_pending());
    // A vertical ridge drawn down the middle (x = 5), short of the end walls.
    c.apply_roof_area_ridge(&[(5.0, 3.0), (5.0, 7.0)]).unwrap();
    assert!(c.roof_area_pitch_pending());

    c.assign_roof_area_pitch(45.0).unwrap();
    assert!(!c.roof_area_pitch_pending(), "committed");

    let ber = &c.current_record.as_ref().unwrap().data;
    // The CEILING is untouched — no apexes were stamped.
    assert!(
        ber.storeys[0].ceiling_apexes.is_empty(),
        "the roof must NOT reshape the ceiling"
    );
    // The roof landed on Ber.roof as a separate area.
    assert_eq!(ber.roof.areas.len(), 1, "one roof area on Ber.roof");
    let area = &ber.roof.areas[0];
    assert_eq!(area.shape, ber_model::ber::roof::RoofShape::Gable);
    assert!((area.pitch_deg - 45.0).abs() < 1e-4);
    assert!(
        (area.eave_z - 2.4).abs() < 1e-6,
        "eave at the ceiling height"
    );
    let (a, b) = area.ridge.expect("gable has a ridge");
    assert!(
        (a.x - 5.0).abs() < 1e-3 && (b.x - 5.0).abs() < 1e-3,
        "ridge at x = 5"
    );
    let mut ys = [a.y, b.y];
    ys.sort_by(|p, q| p.partial_cmp(q).unwrap());
    assert!(
        ys[0].abs() < 1e-3 && (ys[1] - 10.0).abs() < 1e-3,
        "a gable extends the ridge to the area span (y 0..10): {ys:?}"
    );
    assert!(
        !area.footprint.is_empty(),
        "the covered footprint is recorded"
    );
}

/// A HIP keeps the DRAWN ridge length (the ends slope in) rather than extending
/// to the end walls — the recorded ridge endpoints are the drawn 3 / 7. The hip
/// flow now COLLECTS ridges (an L-shaped hip has several): after the stroke it
/// stays in the ridge stage until "Finish ridges" moves it to the pitch.
#[test]
fn roof_area_hip_keeps_drawn_ridge_length() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.set_roof_mode(true);
    c.confirm_all_storeys_added_roof();
    c.roof_region_tap(5.0, 5.0, 0.5);
    c.pick_roof_area_shape(RoofShape::Hip);
    c.apply_roof_area_ridge(&[(5.0, 3.0), (5.0, 7.0)]).unwrap();
    assert!(
        c.roof_area_ridge_pending() && c.roof_area_hip_ridge_count() == Some(1),
        "a hip stays collecting ridges after a stroke"
    );
    c.finish_roof_area_ridges().unwrap();
    assert!(c.roof_area_pitch_pending(), "Finish opens the pitch");
    c.assign_roof_area_pitch(45.0).unwrap();

    let ber = &c.current_record.as_ref().unwrap().data;
    assert!(
        ber.storeys[0].ceiling_apexes.is_empty(),
        "ceiling untouched"
    );
    let area = &ber.roof.areas[0];
    assert_eq!(area.shape, ber_model::ber::roof::RoofShape::Hip);
    let (a, b) = area.ridge.expect("hip has a ridge");
    let mut ys = [a.y, b.y];
    ys.sort_by(|p, q| p.partial_cmp(q).unwrap());
    assert!(
        (ys[0] - 3.0).abs() < 1e-3 && (ys[1] - 7.0).abs() < 1e-3,
        "a hip keeps the drawn ridge extent (y 3..7): {ys:?}"
    );
}

/// A HIP accepts SEVERAL ridge strokes before Finish — each contributes its
/// apex pair to the committed area (4 apexes for 2 ridges), and finishing with
/// no ridge drawn is refused.
#[test]
fn roof_area_hip_collects_multiple_ridges() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.set_roof_mode(true);
    c.confirm_all_storeys_added_roof();
    c.roof_region_tap(5.0, 5.0, 0.5);
    c.pick_roof_area_shape(RoofShape::Hip);
    assert!(
        c.finish_roof_area_ridges().is_err(),
        "finishing with no ridge drawn is refused"
    );
    c.apply_roof_area_ridge(&[(5.0, 2.0), (5.0, 5.0)]).unwrap();
    c.apply_roof_area_ridge(&[(3.0, 5.0), (5.0, 5.0)]).unwrap();
    assert_eq!(c.roof_area_hip_ridge_count(), Some(2));
    assert_eq!(
        c.pending_roof_area_ridge_lines().len(),
        2,
        "both collected ridges overlay while drawing"
    );
    c.finish_roof_area_ridges().unwrap();
    c.assign_roof_area_pitch(45.0).unwrap();

    let area = &c.current_record.as_ref().unwrap().data.roof.areas[0];
    assert_eq!(
        area.apexes.len(),
        4,
        "one apex pair per ridge: {:?}",
        area.apexes
    );
    assert!(area.ridge.is_some(), "the first ridge is recorded");
}

/// A HIP's DIAGONAL ridge stroke is NOT orthogonalized: drawn at ~45° from
/// near a footprint corner towards the main ridge's end, it keeps its
/// direction, its corner end snaps onto the corner, and its inner end snaps
/// onto the already-drawn main ridge's endpoint. Near-orthogonal strokes
/// still straighten (the main ridge below).
#[test]
fn roof_area_hip_diagonal_ridge_is_not_orthogonalized() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.set_roof_mode(true);
    c.confirm_all_storeys_added_roof();
    c.roof_region_tap(5.0, 5.0, 0.5);
    c.pick_roof_area_shape(RoofShape::Hip);
    // Main ridge: slightly wobbly vertical stroke → straightens to x = 5.
    c.apply_roof_area_ridge(&[(4.9, 3.0), (5.1, 7.0)]).unwrap();
    // Corner ridge: ~45° from near the (10,10) corner to the main ridge's
    // (5,7) end — must stay diagonal, ends snapped to corner + junction.
    c.apply_roof_area_ridge(&[(9.8, 9.7), (5.2, 7.3)]).unwrap();
    c.finish_roof_area_ridges().unwrap();
    c.assign_roof_area_pitch(35.0).unwrap();

    let area = &c.current_record.as_ref().unwrap().data.roof.areas[0];
    assert_eq!(area.apexes.len(), 4, "two ridges, one apex pair each");
    let has_apex_at = |x: f64, y: f64| {
        area.apexes
            .iter()
            .any(|a| (a.point.x - x).abs() < 1e-6 && (a.point.y - y).abs() < 1e-6)
    };
    assert!(
        has_apex_at(5.0, 3.0) && has_apex_at(5.0, 7.0),
        "the main ridge straightened to x = 5: {:?}",
        area.apexes
    );
    assert!(
        has_apex_at(10.0, 10.0),
        "the diagonal's corner end snapped onto (10,10): {:?}",
        area.apexes
    );
    assert!(
        has_apex_at(5.0, 7.0),
        "the diagonal's inner end meets the main ridge at (5,7)"
    );
}

/// The GABLE pitch stage offers "Centre on roof": a rough off-centre stroke
/// (x = 3 across the 10-wide square) recentres to x = 5, keeping its
/// full-span extension along the ridge.
#[test]
fn roof_area_gable_ridge_can_be_centred() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.set_roof_mode(true);
    c.confirm_all_storeys_added_roof();
    c.roof_region_tap(5.0, 5.0, 0.5);
    c.pick_roof_area_shape(RoofShape::Gable);
    c.apply_roof_area_ridge(&[(3.0, 3.0), (3.0, 7.0)]).unwrap();
    assert!(c.roof_area_can_centre_ridge(), "gable offers centring");
    // The pending 2D line shows the DRAWN (pre-centre) position first…
    let lines = c.pending_roof_area_ridge_lines();
    assert!(
        (lines[0].0 .0 - 3.0).abs() < 1e-3,
        "pending line at the drawn x = 3: {lines:?}"
    );
    c.centre_pending_roof_ridge();
    // …and moves to the centre IMMEDIATELY on the button press, so the canvas
    // redraws the green line centred before the pitch is even entered.
    let lines = c.pending_roof_area_ridge_lines();
    assert!(
        (lines[0].0 .0 - 5.0).abs() < 1e-3 && (lines[0].1 .0 - 5.0).abs() < 1e-3,
        "pending line recentred to x = 5: {lines:?}"
    );
    c.assign_roof_area_pitch(45.0).unwrap();

    let area = &c.current_record.as_ref().unwrap().data.roof.areas[0];
    let (a, b) = area.ridge.expect("gable has a ridge");
    assert!(
        (a.x - 5.0).abs() < 1e-3 && (b.x - 5.0).abs() < 1e-3,
        "recentred to x = 5: {a:?} {b:?}"
    );
    // Centred half-span run: eave 2.4 + 5·tan45 = 7.4.
    assert!(
        area.apexes.iter().all(|ap| (ap.z_m - 7.4).abs() < 1e-6),
        "apex height from the centred run: {:?}",
        area.apexes
    );
}

/// A ridge drawn NEARLY on an eave edge snaps ONTO it (mono-pitch): drawn at
/// x = 9.8 against the x = 10 wall, the committed ridge sits at 10 and its
/// apexes rise over the FULL 10 m span.
#[test]
fn roof_area_ridge_near_an_edge_snaps_onto_it() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.set_roof_mode(true);
    c.confirm_all_storeys_added_roof();
    c.roof_region_tap(5.0, 5.0, 0.5);
    c.pick_roof_area_shape(RoofShape::Gable);
    c.apply_roof_area_ridge(&[(9.8, 3.0), (9.8, 7.0)]).unwrap();
    c.assign_roof_area_pitch(45.0).unwrap();

    let area = &c.current_record.as_ref().unwrap().data.roof.areas[0];
    let (a, b) = area.ridge.expect("gable has a ridge");
    assert!(
        (a.x - 10.0).abs() < 1e-3 && (b.x - 10.0).abs() < 1e-3,
        "snapped onto the x = 10 edge: {a:?} {b:?}"
    );
    // Mono-pitch run is the full span: eave 2.4 + 10·tan45 = 12.4.
    assert!(
        area.apexes.iter().all(|ap| (ap.z_m - 12.4).abs() < 1e-6),
        "apex height from the full-span run: {:?}",
        area.apexes
    );
}

/// A FLAT roof commits immediately (no ridge/pitch) to `Ber.roof`, ceiling
/// untouched.
#[test]
fn roof_area_flat_commits_without_ridge() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.set_roof_mode(true);
    c.confirm_all_storeys_added_roof();
    c.roof_region_tap(5.0, 5.0, 0.5);
    c.pick_roof_area_shape(RoofShape::Flat);

    let ber = &c.current_record.as_ref().unwrap().data;
    assert!(
        ber.storeys[0].ceiling_apexes.is_empty(),
        "ceiling untouched"
    );
    assert_eq!(ber.roof.areas.len(), 1);
    let area = &ber.roof.areas[0];
    assert_eq!(area.shape, ber_model::ber::roof::RoofShape::Flat);
    assert!(area.ridge.is_none(), "a flat roof has no ridge");
    assert!(!c.roof_area_pitch_pending(), "no pitch question for flat");
}

/// Converted attic: when the covered storey's ceiling is already sloped, the roof
/// step offers "Follow ceiling", and picking it commits a roof that ADOPTS the
/// ceiling's apexes (so the roof coincides with the sloped ceiling) — the ceiling
/// itself stays untouched.
#[test]
fn roof_follows_a_sloped_ceiling_in_a_converted_attic() {
    use ber_model::ber::structural::CeilingApex;
    use domain::points::Point2f64;

    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    // Slope the ceiling directly (a ridge at x = 5, height 6.9), then re-derive.
    {
        let rec = c.current_record.as_mut().unwrap();
        rec.data.add_ceiling_apex(
            0,
            CeilingApex {
                point: Point2f64::new(5.0, 0.0),
                z_m: 6.9,
            },
        );
        rec.data.add_ceiling_apex(
            0,
            CeilingApex {
                point: Point2f64::new(5.0, 10.0),
                z_m: 6.9,
            },
        );
    }
    c.recompose_ceilings(0);
    c.materialize();

    c.set_roof_mode(true);
    c.confirm_all_storeys_added_roof();
    // Tap inside the west slope (off the ridge line, which is a shared edge).
    c.roof_region_tap(2.5, 5.0, 0.5);
    assert!(c.roof_area_can_pick_shape(), "a ceiling area was grouped");
    assert!(
        c.roof_area_ceiling_is_sloped(),
        "the sloped ceiling is detected"
    );
    c.pick_roof_area_shape(RoofShape::FollowCeiling);

    let ber = &c.current_record.as_ref().unwrap().data;
    assert_eq!(
        ber.storeys[0].ceiling_apexes.len(),
        2,
        "the ceiling itself is untouched"
    );
    let area = &ber.roof.areas[0];
    assert_eq!(area.shape, ber_model::ber::roof::RoofShape::FollowCeiling);
    assert_eq!(
        area.apexes.len(),
        2,
        "the roof adopted the ceiling's apexes"
    );
    assert!(
        area.apexes.iter().all(|a| (a.z_m - 6.9).abs() < 1e-6),
        "same apex height as the ceiling"
    );
}

/// A tap within snap tolerance of a parallel wall on ONE axis snaps that
/// axis: its position prompt is SKIPPED (the coordinate is a reference
/// wall's, exact by construction), the single tag renders, the OTHER axis is
/// still asked, and the assigned apex lands exactly on the wall's coordinate.
#[test]
fn ceiling_point_snapped_axis_skips_its_prompt_and_lands_exactly() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    assert!(c.ceiling_point_mode());
    // Tap (3.0, 3.9): Y is within 0.2 of the interior wall y = 4 → snaps;
    // X has no wall within tolerance → asked.
    c.ceiling_point_tap(3.0, 3.9);
    let tag = c.ceiling_point_snap_tag().expect("snap tag present");
    assert_eq!(tag.current_kind, Some(manip_plan::SnapKind::ToParallelWall));
    assert!((tag.pos.1 - 4.0).abs() < 1e-6, "tag at the snapped point");
    assert!(
        (tag.from.1 - 3.9).abs() < 1e-6,
        "identity = the tapped point"
    );
    assert!(
        tag.options.is_empty() && tag.skippable,
        "menu = Don't snap only"
    );

    let ui = c.position_prompt_for_ui().expect("X still asked");
    assert_eq!(ui.kind, PositionPromptKind::Point);
    assert_eq!(ui.axis, ParameterAxis::X);
    // Nearest ref is the interior wall x = 6 (3 m), drawn side −:
    // 2.5 m from it → x = 3.5.
    c.apply_position_dimension(2.5).expect("X applies");
    assert!(
        c.ceiling_point_height_pending(),
        "snapped Y skipped its prompt — straight to the height"
    );
    assert!(c.ceiling_point_snap_tag().is_some(), "tag still clickable");

    c.assign_roof_apex(4.0).expect("apex assigns");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let apex = &storey.ceiling_apexes[0];
    assert!((apex.point.x - 3.5).abs() < 1e-6);
    assert!(
        (apex.point.y - 4.0).abs() < 1e-6,
        "snapped axis lands EXACTLY"
    );
}

/// Both axes within tolerance = a corner snap for free: no position prompt
/// at all, straight to the height, and the single tag shows the corner kind.
#[test]
fn ceiling_point_both_axes_snapped_goes_straight_to_height() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    c.ceiling_point_tap(5.9, 3.9); // within 0.2 of x = 6 AND y = 4
    assert!(
        c.position_prompt_for_ui().is_none(),
        "both snapped — no position question"
    );
    assert!(c.ceiling_point_height_pending(), "straight to the height");
    assert_eq!(
        c.pending_roof_point_position(),
        Some((6.0, 4.0)),
        "both coordinates snapped"
    );
    let tag = c.ceiling_point_snap_tag().expect("one tag for both");
    assert_eq!(tag.current_kind, Some(manip_plan::SnapKind::ToCorner));
}

/// "Don't snap" from the HEIGHT stage reverts BOTH axes to their tapped
/// coordinates and restarts the question flow from X.
#[test]
fn unsnap_roof_point_restarts_from_x_with_the_drawn_coords() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    c.ceiling_point_tap(5.9, 3.9);
    assert!(c.ceiling_point_height_pending());

    c.unsnap_roof_point();
    assert!(c.ceiling_point_snap_tag().is_none(), "snaps dropped");
    let ui = c.position_prompt_for_ui().expect("X question reopened");
    assert_eq!(ui.kind, PositionPromptKind::Point);
    assert_eq!(ui.axis, ParameterAxis::X);
    assert_eq!(
        c.pending_roof_point_position(),
        Some((5.9 as f32 as f64, 3.9 as f32 as f64)),
        "both axes back at the DRAWN coordinates"
    );
    // The restarted flow works as normal: skip X, then Y is asked too
    // (un-snapped now), skip it, and the height opens on the drawn point.
    c.skip_position_dimension().unwrap();
    let ui = c.position_prompt_for_ui().expect("Y question follows");
    assert_eq!(ui.axis, ParameterAxis::Y);
    c.skip_position_dimension().unwrap();
    assert!(c.ceiling_point_height_pending());
}

/// Reload rebuilds the fan from the DURABLE apex with NO session state, and
/// a no-edit re-derive reproduces it exactly — the apex reload fixpoint.
#[test]
fn ceiling_apex_survives_reload_with_no_session_state_and_rederive_fixpoint() {
    let mut c = ceiling_apex_ready_controller();
    c.ceiling_point_tap(3.8, 5.2);
    c.apply_position_dimension(4.0).unwrap();
    c.apply_position_dimension(5.0).unwrap();
    c.assign_roof_apex(4.2).unwrap();
    c.materialize();

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceilings.regions.len(), 4, "fan persisted");
    assert_eq!(storey.ceiling_apexes.len(), 1, "apex input persisted");
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "no session state to reconstruct for a default-base fan"
    );

    // Re-derive without any edit: the fan must be reproduced exactly.
    c.mark_dirty().unwrap();
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(
        storey.ceilings.regions.len(),
        4,
        "fan kept across the re-derive"
    );
    let max_z = storey
        .ceilings
        .regions
        .iter()
        .flat_map(|r| r.polygon.exterior.iter().map(|v| v.z))
        .fold(f64::NEG_INFINITY, f64::max);
    assert!((max_z - 4.2).abs() < 1e-6, "apex height kept");
}

/// An apex OVER a ceiling step — the load-bearing fixpoint chain end-to-end:
/// the fan keeps the stepped base, the session ceiling-step list
/// CANONICALIZES into ONE merged step area (the fan's apex group, merged
/// back to the whole stepped piece), and reload + re-derive reproduce the
/// identical surface.
#[test]
fn ceiling_apex_over_a_ceiling_step_canonicalizes_and_survives_reload() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).unwrap(); // right of x = 4 at 2.7
    c.set_roof_point_mode(true); // leaves ceiling-step mode; sessions stay

    // Apex inside the STEPPED piece, at its drawn position (skip both axes).
    c.ceiling_point_tap(7.0, 5.0);
    c.skip_position_dimension().unwrap(); // X stays 7.0
    c.skip_position_dimension().unwrap(); // Y stays 5.0
    c.assign_roof_apex(4.2).expect("apex over the step assigns");

    // Canonicalized: the fan's apex group merges back into ONE step area
    // covering the whole stepped piece at the step delta.
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "one merged area, not fan triangles");
    assert!((session[0].delta_m - 0.3).abs() < 1e-6, "keeps the delta");
    let xs: Vec<f32> = session[0].polygon.iter().map(|p| p.x).collect();
    let min_x = xs.iter().copied().fold(f32::INFINITY, f32::min);
    let max_x = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    assert!((min_x - 4.0).abs() < 1e-4 && (max_x - 10.0).abs() < 1e-4);
    c.materialize();
    let zs_of = |c: &AppController| {
        let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
        let mut zs: Vec<f64> = storey
            .ceilings
            .regions
            .iter()
            .flat_map(|r| r.polygon.exterior.iter().map(|v| v.z))
            .collect();
        zs.sort_by(f64::total_cmp);
        zs
    };
    let before = zs_of(&c);
    assert!((before.last().unwrap() - 4.2).abs() < 1e-6, "apex present");
    assert!(
        before.iter().any(|z| (z - 2.7).abs() < 1e-6),
        "fan base kept the STEPPED height (apex-over-step)"
    );
    assert!(
        before.iter().any(|z| (z - 2.4).abs() < 1e-6),
        "the un-stepped piece stays at the default"
    );

    // Reload: session list reconstructs to the same triangles; the surface
    // re-derives identically (the fixpoint).
    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(
        session.len(),
        1,
        "merged area reconstructed from the ceilings"
    );
    assert!(session.iter().all(|r| (r.delta_m - 0.3).abs() < 1e-6));
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        zs_of(&c),
        before,
        "derive → reconstruct → derive is a fixpoint"
    );
}

/// Deleting the apex re-flattens its fan to the base on the next derive
/// (and clears the now-empty session step list — same canonicalize).
#[test]
fn deleting_a_roof_apex_reflattens_the_fan() {
    let mut c = ceiling_apex_ready_controller();
    c.ceiling_point_tap(3.8, 5.2);
    c.apply_position_dimension(4.0).unwrap();
    c.apply_position_dimension(5.0).unwrap();
    c.assign_roof_apex(4.2).unwrap();
    c.materialize();
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .ceilings
            .regions
            .len(),
        4
    );

    c.delete_ceiling_apex(0);
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(storey.ceiling_apexes.is_empty(), "durable input removed");
    assert_eq!(storey.ceilings.regions.len(), 1, "back to one flat region");
    assert!(
        storey
            .ceilings
            .regions
            .iter()
            .all(|r| r.flat_z().is_some_and(|z| (z - 2.4).abs() < 1e-6)),
        "flat at the default again"
    );
    assert!(c.ceiling_step_regions.get(&0).is_none());
    // Out-of-range delete is a no-op.
    c.delete_ceiling_apex(7);
}

/// A storey switch abandons the in-flight point and leaves the mode — its
/// coordinates are in the outgoing storey's space.
#[test]
fn storey_switch_clears_pending_roof_point() {
    let mut c = ceiling_apex_ready_controller();
    c.ceiling_point_tap(3.8, 5.2);
    assert!(c.ceiling_point_pending());
    c.add_storey().unwrap();
    assert!(!c.ceiling_point_mode(), "mode reset on switch");
    assert!(!c.ceiling_point_pending(), "pending point abandoned");
}

/// The two-point ridge flow at the shell level: a SECOND apex inside an
/// already-fanned region is ACCEPTED — the ridge forms (a sloping one here,
/// the two heights differ) — and a THIRD point in the ridged area is
/// rejected with the stable two-per-area message; the pending point stays
/// open so the user can cancel or move on.
#[test]
fn second_roof_point_forms_a_ridge_and_a_third_is_rejected() {
    let mut c = ceiling_apex_ready_controller();
    c.ceiling_point_tap(3.8, 5.2);
    c.apply_position_dimension(4.0).unwrap();
    c.apply_position_dimension(5.0).unwrap();
    c.assign_roof_apex(4.2).unwrap();

    // Second point, strictly inside one of the fan triangles: the ridge.
    c.ceiling_point_tap(3.9, 1.1);
    c.skip_position_dimension().unwrap();
    c.skip_position_dimension().unwrap();
    c.assign_roof_apex(3.5)
        .expect("second point forms the ridge");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 2, "both apexes durable");
    let zs: Vec<f64> = storey
        .ceilings
        .regions
        .iter()
        .flat_map(|r| r.polygon.exterior.iter().map(|v| v.z))
        .collect();
    assert!(
        zs.iter().any(|z| (z - 4.2).abs() < 1e-6),
        "ridge rises to the first apex"
    );
    assert!(
        zs.iter().any(|z| (z - 3.5).abs() < 1e-6),
        "…and carries the second"
    );

    // Third point in the ridged area: rejected, pending stays open.
    c.ceiling_point_tap(8.0, 5.0);
    c.skip_position_dimension().unwrap();
    c.skip_position_dimension().unwrap();
    assert!(c.ceiling_point_height_pending());
    let err = c.assign_roof_apex(3.0).unwrap_err();
    assert!(err.contains("already has a ridge"), "got: {err}");
    assert!(
        c.ceiling_point_height_pending(),
        "pending intact on rejection"
    );
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 2, "nothing committed");
    c.cancel_pending_roof_point();
    assert!(!c.ceiling_point_pending());
}

// ─── Roof ridges (line input: length + cross + start questions + height) ───

/// Sorted (x, y, z) vertex triples of storey 0's derived roof regions —
/// a strong geometric fingerprint for "same roof" comparisons.
fn ceiling_vertex_fingerprint(c: &AppController) -> Vec<(f64, f64, f64)> {
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let mut vs: Vec<(f64, f64, f64)> = storey
        .ceilings
        .regions
        .iter()
        .flat_map(|r| r.polygon.exterior.iter().map(|v| (v.x, v.y, v.z)))
        .collect();
    vs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    vs
}

/// The full un-snapped line flow asks THREE questions in the assessor's
/// measuring order — dimension first, then where it sits — each prefilled
/// with the drawn value (the stroke is approximate): the LENGTH (a typed
/// amount, no reference wall), the CROSS position (kind `Line`, the "area
/// wall" being the segment itself), and the START position along the ridge
/// axis (measured from a PERPENDICULAR wall to the extent's lo — the "area
/// wall" degenerates to the LO endpoint). Typed answers land EXACTLY — the
/// assigned apexes sit at the RESOLVED `(cross, start)`/`(cross, start +
/// length)`, not the drawn ones. One typed height then places BOTH apexes;
/// the derived roof is a pitched ridge (one two-apex group), with no session
/// step state over the default base.
#[test]
fn ceiling_ridge_length_cross_start_height_flow_forms_a_pitched_roof() {
    let mut c = ceiling_apex_ready_controller();
    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);

    // Question 1: the LENGTH — prefilled with the drawn |hi − lo|, no
    // reference wall (it's the ridge's own dimension).
    assert!(
        c.ceiling_ridge_length_pending(),
        "stroke fitted → length first"
    );
    assert!(
        c.position_prompt_for_ui().is_none(),
        "the length is not a position question"
    );
    let prefill = c.pending_roof_ridge_length().expect("drawn length");
    assert!((prefill - 6.0).abs() < 1e-4);
    assert!(
        !c.ceiling_ridge_length_anchored(),
        "nothing snapped — no anchored end"
    );
    // A typed length anchors at the drawn lo: the extent becomes [2, 6].
    c.assign_roof_ridge_length(4.0).expect("length applies");

    // Question 2: the CROSS position.
    assert!(
        c.ceiling_ridge_position_pending(),
        "length resolved → cross"
    );
    let ui = c.position_prompt_for_ui().expect("cross-coord prompt");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    assert_eq!(ui.axis, ParameterAxis::X);
    let ((ax, ay), (bx, by)) = c.pending_roof_ridge_line().expect("segment");
    assert!((ax - 3.8).abs() < 0.05 && (bx - 3.8).abs() < 0.05);
    assert!(
        (ay - 2.0).abs() < 1e-4 && (by - 6.0).abs() < 1e-4,
        "extent = [drawn lo, drawn lo + typed length]"
    );
    assert!(
        (ui.area_wall.wall_a.1 - 2.0).abs() < 1e-4 && (ui.area_wall.wall_b.1 - 6.0).abs() < 1e-4,
        "the prompt's area wall IS the segment"
    );
    // Nearest ref is x = 0 (≈3.8 m): 4 m from it → x = 4. The extent must
    // NOT shift — it's on the other axis.
    c.apply_position_dimension(4.0).expect("cross applies");

    // Question 3: the START position, on the ALONG axis (a vertical ridge's
    // extent runs on Y → the references are HORIZONTAL walls), highlighting
    // the LO ENDPOINT (degenerate wall) at the already-resolved cross.
    assert!(c.ceiling_ridge_position_pending(), "cross resolved → start");
    let ui = c.position_prompt_for_ui().expect("start prompt");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    assert_eq!(ui.axis, ParameterAxis::Y);
    assert_eq!(
        ui.area_wall.wall_a, ui.area_wall.wall_b,
        "the measured object is the LO endpoint — a degenerate wall"
    );
    assert!((ui.area_wall.wall_a.0 - 4.0).abs() < 1e-6, "resolved cross");
    assert!((ui.area_wall.wall_a.1 - 2.0).abs() < 1e-4, "drawn lo");
    assert!(
        (ui.current_distance - 2.0).abs() < 1e-4,
        "prefill = drawn distance from the nearest ref (y = 0)"
    );
    // 3 m from y = 0 → the ridge now STARTS at y = 3; the typed length (4)
    // rides along, so the far end lands at y = 7. The last open location
    // stage runs the finalize placement check on the way out.
    c.apply_position_dimension(3.0).expect("start applies");

    // The height, then assign.
    assert!(c.ceiling_ridge_height_pending(), "start resolved → height");
    let ((_, ay), (_, by)) = c.pending_roof_ridge_line().unwrap();
    assert!(
        (ay - 3.0).abs() < 1e-4 && (by - 7.0).abs() < 1e-4,
        "extent = [typed start, typed start + typed length]"
    );
    c.assign_roof_ridge_height(4.2).expect("ridge assigns");
    assert!(!c.ceiling_ridge_pending(), "flow complete");
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 2, "both apexes durable");
    for (apex, want_y) in storey.ceiling_apexes.iter().zip([3.0, 7.0]) {
        assert!((apex.point.x - 4.0).abs() < 1e-6);
        assert!(
            (apex.point.y - want_y).abs() < 1e-4,
            "apexes at the RESOLVED start/length, not the drawn ones"
        );
        assert!((apex.z_m - 4.2).abs() < 1e-6);
    }
    let groups = storey.ceilings.apex_groups(&storey.ceiling_apexes);
    assert_eq!(groups.len(), 1, "one connected roof group");
    assert_eq!(
        groups[0].apex_idxs.len(),
        2,
        "…holding BOTH apexes: a ridge"
    );
    let zs: Vec<f64> = storey
        .ceilings
        .regions
        .iter()
        .flat_map(|r| r.polygon.exterior.iter().map(|v| v.z))
        .collect();
    assert!(zs.iter().any(|z| (z - 4.2).abs() < 1e-6), "ridge height");
    assert!(
        zs.iter().any(|z| (z - 2.4).abs() < 1e-6),
        "eaves at the base"
    );
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "a ridge over the default base needs NO session step state"
    );
    assert_eq!(
        c.ceiling_ridge_views(),
        vec![((4.0, 3.0), (4.0, 7.0), "⌃ 4.20 m".into())],
        "committed overlay: the segment + level-ridge label"
    );
}

/// A T-shaped storey: a wide bottom bar (x ∈ 0..12, y ∈ 0..3) plus a narrow
/// top stub (x ∈ 5..7, y ∈ 3..10). Vertical (X) outline walls at x ∈ {0,5,7,12}
/// — note the INTERIOR stub walls x ∈ {5,7} sit between the bottom bar's
/// bounding walls x ∈ {0,12}. Default ceiling height set, roof-point mode on.
fn ceiling_ready_t_controller() -> AppController {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    c.document.floor_plan = FloorPlan::from_clean_ring(&[
        p(0., 0.),
        p(12., 0.),
        p(12., 3.),
        p(7., 3.),
        p(7., 10.),
        p(5., 10.),
        p(5., 3.),
        p(0., 3.),
    ])
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
    c.set_default_ceiling_height(2.4).expect("height applies");
    c.set_roof_point_mode(true);
    assert!(c.ceiling_point_mode());
    c
}

/// B-T5: on a same-height-SPLIT T storey, a ridge drawn in the bottom bar gets
/// a cross-position prompt whose DEFAULT reference wall bounds the ridge's OWN
/// region (x = 12, a bottom-bar bbox edge) — NOT the globally-nearest interior
/// stub wall (x = 7), which doesn't bound the bottom bar. Cycling still reaches
/// every wall (the option set is unchanged). The default-wall smartness is the
/// only change; the rest of the flow is unaffected.
#[test]
fn ceiling_ridge_default_wall_bounds_its_own_arm() {
    let mut c = ceiling_ready_t_controller();
    // Same-height split at y = 3 (Less side = the bottom bar): the bottom bar
    // and the top stub become two flat-default regions, so the bottom bar is a
    // region of its OWN that `ridge_region_ring` can return. (Direct push +
    // canonicalize — the `stepped_ceiling_below_controller` pattern, delta 0.)
    let divider = c
        .document
        .ceiling_step_region(ParameterAxis::Y, 3.0, document::LineSide::Less, 0.0)
        .expect("divider region builds");
    c.ceiling_step_regions.entry(0).or_default().push(divider);
    c.mark_dirty().unwrap();
    c.recompose_ceilings(0); // regions-as-truth: compose the divider into the ceilings
    c.materialize();
    c.reconstruct_ceiling_step_regions_for(0);
    assert_eq!(ceiling_flat_zs(&c).len(), 2, "two same-height regions");

    // A vertical ridge in the bottom bar at cross x ≈ 8 (1 m from the interior
    // stub wall x = 7 — beyond the 0.2 m snap tol, so the cross question
    // opens), extent y ∈ [0.5, 2.5] (midpoint (8, 1.5) strictly inside the
    // bottom bar). No end snaps (0.5 m clear of y ∈ {0,3}).
    c.apply_roof_ridge_stroke_event(&[(8.0, 0.5), (8.02, 1.5), (8.0, 2.5)]);
    assert!(
        c.ceiling_ridge_length_pending(),
        "stroke fitted → length first"
    );
    c.skip_roof_ridge_length().expect("accept the drawn length");

    // The cross question, with the SMART default: x = 12 bounds the bottom bar;
    // the global-nearest x = 7 (interior stub wall) does not.
    assert!(c.ceiling_ridge_position_pending(), "length → cross");
    let ui = c.position_prompt_for_ui().expect("cross prompt");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    assert_eq!(ui.axis, ParameterAxis::X);
    assert!(
        (ui.reference_wall.wall_a.0 - 12.0).abs() < 1e-4,
        "default reference wall bounds the ridge's own arm (x = 12), not the \
         globally-nearest interior wall x = 7 (got {})",
        ui.reference_wall.wall_a.0
    );
    assert_eq!(
        ui.option_count, 4,
        "the full option set is preserved (x ∈ 0,5,7,12)"
    );

    // Cycling reaches the others — including the interior x = 7 that was the
    // global nearest — proving only the DEFAULT changed, not the set.
    let mut seen = vec![ui.reference_wall.wall_a.0.round()];
    for _ in 0..ui.option_count - 1 {
        c.cycle_position_reference();
        let u = c.position_prompt_for_ui().expect("cycled prompt");
        seen.push(u.reference_wall.wall_a.0.round());
    }
    seen.sort_by(f64::total_cmp);
    assert_eq!(seen, vec![0.0, 5.0, 7.0, 12.0], "cycle reaches every wall");
}

/// B-T5 (smoke): the three NON-ridge callers that pass `None` keep their
/// global-nearest default — the ridge smartness doesn't bleed into them. A
/// ceiling-step line on the bottom bar defaults to the globally-nearest wall.
#[test]
fn non_ridge_position_prompts_stay_global_nearest() {
    let mut c = ceiling_ready_t_controller();
    c.set_roof_point_mode(false);
    c.set_ceiling_step_mode(true);
    // Vertical ceiling-step line at x ≈ 8 — nearest X-wall is the interior
    // x = 7 (1 m), the global-nearest default (None → no priority ring).
    c.apply_ceiling_step_stroke_event(&[(8.0, 0.5), (8.02, 5.0), (8.0, 9.5)]);
    assert!(c.ceiling_step_side_pending());
    c.ceiling_step_tap(2.0, 1.0); // Less side
    let ui = c
        .position_prompt_for_ui()
        .expect("ceiling-step line prompt");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    assert!(
        (ui.reference_wall.wall_a.0 - 7.0).abs() < 1e-4,
        "the ceiling-step line stays global-nearest (x = 7), not arm-aware"
    );
}

/// The line input and the two-point tap flow are the SAME model: skipping
/// every question keeps the DRAWN values (cross, start, AND length), and
/// placing the two apexes by taps at exactly those drawn endpoints derives a
/// vertex-for-vertex identical roof.
#[test]
fn ceiling_ridge_line_input_matches_the_two_point_flow() {
    // Line input with every prompt skipped: the drawn segment stands.
    let mut line = ceiling_apex_ready_controller();
    line.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    let ((ax, ay), (bx, by)) = line.pending_roof_ridge_line().expect("drawn segment");
    line.skip_roof_ridge_length().unwrap(); // length: drawn |hi − lo| stands
    line.skip_position_dimension().unwrap(); // cross: drawn coordinate stands
    line.skip_position_dimension().unwrap(); // start: drawn lo stands
    line.assign_roof_ridge_height(4.2).unwrap();
    line.materialize();
    let ((lax, lay), (lbx, lby)) = ((ax, ay), (bx, by));

    // Two-point flow at the same (drawn) coordinates and height.
    let mut taps = ceiling_apex_ready_controller();
    for (x, y) in [(lax, lay), (lbx, lby)] {
        taps.ceiling_point_tap(x as f32, y as f32);
        taps.skip_position_dimension().unwrap(); // X stays as tapped
        taps.skip_position_dimension().unwrap(); // Y stays as tapped
        taps.assign_roof_apex(4.2).expect("apex assigns");
    }
    taps.materialize();

    let a = ceiling_vertex_fingerprint(&line);
    let b = ceiling_vertex_fingerprint(&taps);
    assert_eq!(a.len(), b.len(), "same number of roof vertices");
    for (va, vb) in a.iter().zip(&b) {
        assert!(
            (va.0 - vb.0).abs() < 1e-9 && (va.1 - vb.1).abs() < 1e-9 && (va.2 - vb.2).abs() < 1e-9,
            "same roof, vertex for vertex ({va:?} vs {vb:?})"
        );
    }
}

/// A stroke within snap tolerance of a parallel wall SNAPS the cross
/// coordinate onto it: after the Length question (always first) the CROSS
/// question is SKIPPED (the coordinate is exact by construction) — the flow
/// moves straight to the START question — the single tag renders at the
/// segment midpoint from then on (the stage owns the tag: none shows at the
/// Length stage, where no snap question has been passed yet), and the
/// assigned apexes' cross coordinate equals the wall's EXACTLY.
#[test]
fn ceiling_ridge_snap_skips_the_cross_question_and_lands_on_the_wall() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    assert!(c.ceiling_point_mode());
    // Drawn at x ≈ 5.9 — within 0.2 of the interior wall at x = 6; the
    // extent (y 0.5..3.5) stays inside the L's lower band, beyond snap
    // tolerance of any horizontal wall.
    c.apply_roof_ridge_stroke_event(&[(5.88, 0.5), (5.92, 2.0), (5.9, 3.5)]);
    assert!(
        c.ceiling_ridge_length_pending(),
        "length is always question 1"
    );
    let ((ax, _), _) = c.pending_roof_ridge_line().unwrap();
    assert!((ax - 6.0).abs() < 1e-6, "coordinate snapped to the wall");
    assert!(
        c.ceiling_ridge_snap_tag().is_none(),
        "no tag at the Length stage — no snap question passed yet"
    );

    c.skip_roof_ridge_length().unwrap(); // length: drawn length stands
    let ui = c
        .position_prompt_for_ui()
        .expect("cross snapped → the START question opens directly");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    assert_eq!(ui.axis, ParameterAxis::Y, "the along axis's question");

    let tag = c.ceiling_ridge_snap_tag().expect("snap tag present");
    assert_eq!(tag.current_kind, Some(manip_plan::SnapKind::ToParallelWall));
    assert!((tag.pos.0 - 6.0).abs() < 1e-4, "tag on the snapped segment");
    assert!((tag.pos.1 - 2.0).abs() < 1e-4, "…at its midpoint");
    assert!(
        (tag.from.0 - 5.9).abs() < 0.05,
        "identity = the drawn point"
    );
    assert!(
        tag.options.is_empty() && tag.skippable,
        "menu = Don't snap only"
    );

    c.skip_position_dimension().unwrap(); // start: drawn lo stands → finalize
    assert!(c.ceiling_ridge_height_pending());
    assert!(c.ceiling_ridge_snap_tag().is_some(), "tag still clickable");
    c.assign_roof_ridge_height(4.0).expect("ridge assigns");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 2);
    for (apex, want_y) in storey.ceiling_apexes.iter().zip([0.5, 3.5]) {
        assert!(
            (apex.point.x - 6.0).abs() < 1e-9,
            "snapped cross coordinate lands EXACTLY on the wall"
        );
        assert!((apex.point.y - want_y).abs() < 1e-4, "drawn extent kept");
    }
}

/// A stroke whose LO endpoint lands within snap tolerance of a PERPENDICULAR
/// wall snaps the START: its question is skipped (cross → straight through
/// finalize to the height), the start tag renders at the LO endpoint from
/// the Height stage, the Length question reports the extent ANCHORED, and
/// the drawn LENGTH rides unchanged (snapping slides the extent, it doesn't
/// stretch it). "Don't snap" on the start tag reverts `lo` and reopens the
/// start question only (the resolved length + cross stand).
#[test]
fn ceiling_ridge_start_snap_skips_its_question_and_unsnap_reopens_it() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    // Vertical stroke at x ≈ 2.8 (no parallel wall within 0.2), starting at
    // y ≈ 4.1 — within 0.2 of the horizontal wall y = 4 — ending at y = 8.5.
    c.apply_roof_ridge_stroke_event(&[(2.78, 4.1), (2.82, 6.0), (2.8, 8.5)]);

    // Question 1 (length) opens first; the snapped start ANCHORS the
    // extent (a typed length extends from the exact wall coordinate); no
    // tag yet — the stage owns the tag, and no snap question has arrived.
    assert!(c.ceiling_ridge_length_pending(), "length first");
    assert!(
        c.ceiling_ridge_length_anchored(),
        "start snapped → anchored"
    );
    assert!(c.ceiling_ridge_snap_tag().is_none(), "no tag at the length");
    let ((_, ay), (_, by)) = c.pending_roof_ridge_line().unwrap();
    assert!((ay - 4.0).abs() < 1e-6, "lo snapped onto the wall");
    assert!(
        (by - 8.4).abs() < 1e-4,
        "drawn length (4.4) slides with the snap, it doesn't stretch"
    );
    c.skip_roof_ridge_length().unwrap(); // drawn 4.4 stands

    // Question 2 (cross) opens — still no tag (the start's question
    // hasn't arrived; the cross stage implies an un-snapped cross).
    let ui = c.position_prompt_for_ui().expect("cross question");
    assert_eq!(ui.axis, ParameterAxis::X);
    assert!(c.ceiling_ridge_snap_tag().is_none(), "no tag at the cross");
    c.apply_position_dimension(2.5).expect("cross applies"); // from x = 0

    // The start question is SKIPPED (snapped) → finalize → height.
    assert!(
        c.position_prompt_for_ui().is_none(),
        "snapped start skips its question"
    );
    assert!(c.ceiling_ridge_height_pending(), "straight to the height");
    let ((ax, ay), _) = c.pending_roof_ridge_line().unwrap();
    assert!(
        (ax - 2.5).abs() < 1e-6 && (ay - 4.0).abs() < 1e-6,
        "snapped"
    );
    let tag = c.ceiling_ridge_snap_tag().expect("start tag present");
    assert!(
        (tag.pos.0 - 2.5).abs() < 1e-4 && (tag.pos.1 - 4.0).abs() < 1e-4,
        "tag at the LO endpoint"
    );
    assert!(
        (tag.from.1 - 4.1).abs() < 0.05,
        "identity = the drawn start"
    );

    // "Don't snap": lo reverts to the drawn 4.1 and ONLY the start
    // question reopens (the typed cross AND the resolved length stand).
    c.unsnap_roof_ridge();
    assert!(c.ceiling_ridge_snap_tag().is_none(), "snap dropped");
    let ui = c.position_prompt_for_ui().expect("start question reopened");
    assert_eq!(ui.axis, ParameterAxis::Y);
    let ((ax, ay), _) = c.pending_roof_ridge_line().unwrap();
    assert!((ax - 2.5).abs() < 1e-6, "resolved cross kept");
    assert!((ay - 4.1).abs() < 0.05, "back at the DRAWN start");

    // Type the start (5 m from the default ref y = 0, drawn side +): the
    // apexes land at the RESOLVED start with the resolved (drawn) length.
    // (The default ref is y = 0 — a wall bounding the ridge's own region;
    // the interior notch wall y = 4 doesn't bound the whole-L region, so the
    // smarter-default sort puts it after the boundary walls.)
    c.apply_position_dimension(5.0).expect("start applies"); // y = 5
    c.assign_roof_ridge_height(4.0).expect("ridge assigns");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    for (apex, want_y) in storey.ceiling_apexes.iter().zip([5.0, 9.4]) {
        assert!((apex.point.x - 2.5).abs() < 1e-6);
        assert!((apex.point.y - want_y).abs() < 1e-4);
    }
}

/// "Don't snap" on the CROSS tag reverts the cross coordinate to its DRAWN
/// value and RESTARTS from the cross question (re-asking the start question
/// — the roof point's restart reasoning; the resolved LENGTH stands, its
/// question came first); the reopened flow works as normal (skip everything
/// keeps the drawn segment, assign lands there).
#[test]
fn unsnap_roof_ridge_cross_reopens_the_cross_question_with_the_drawn_coord() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    c.apply_roof_ridge_stroke_event(&[(5.88, 0.5), (5.92, 2.0), (5.9, 3.5)]);
    // Cross snapped: pass the length, then the start question (the cross's
    // was skipped) — the cross tag stays clickable through to the height.
    c.skip_roof_ridge_length().unwrap();
    c.skip_position_dimension().unwrap();
    assert!(c.ceiling_ridge_height_pending());
    assert!(
        c.ceiling_ridge_snap_tag().is_some(),
        "cross tag at the height"
    );

    c.unsnap_roof_ridge();
    assert!(c.ceiling_ridge_snap_tag().is_none(), "snap dropped");
    let ui = c.position_prompt_for_ui().expect("cross question reopened");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    assert_eq!(ui.axis, ParameterAxis::X, "restarted from the cross");
    let ((ax, _), _) = c.pending_roof_ridge_line().unwrap();
    assert!((ax - 5.9).abs() < 0.05, "back at the DRAWN coordinate");

    c.skip_position_dimension().unwrap(); // cross: drawn stands
    c.skip_position_dimension().unwrap(); // start re-asked: drawn stands
    assert!(
        c.ceiling_ridge_height_pending(),
        "skips → height, drawn values (the resolved length stood)"
    );
    c.assign_roof_ridge_height(4.0).expect("ridge assigns");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(
        storey
            .ceiling_apexes
            .iter()
            .all(|a| (a.point.x - 5.9).abs() < 0.05),
        "assigned at the drawn coordinate"
    );
}

/// Reload rebuilds the ridge from the DURABLE apex pair with no session
/// state, and a no-edit re-derive reproduces the surface exactly — the ridge
/// reload fixpoint.
#[test]
fn ceiling_ridge_survives_reload_and_rederive_fixpoint() {
    let mut c = ceiling_apex_ready_controller();
    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    c.assign_roof_ridge_length(6.0).unwrap();
    c.apply_position_dimension(4.0).unwrap(); // cross
    c.apply_position_dimension(2.0).unwrap(); // start (y = 2)
    c.assign_roof_ridge_height(4.2).unwrap();
    c.materialize();
    let before = ceiling_vertex_fingerprint(&c);

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 2, "apex pair persisted");
    let groups = storey.ceilings.apex_groups(&storey.ceiling_apexes);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].apex_idxs.len(), 2, "still a ridge after reload");
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "no session state to reconstruct for a default-base ridge"
    );
    assert_eq!(c.ceiling_ridge_views().len(), 1, "overlay re-derives");

    // Re-derive without any edit: the ridge must be reproduced exactly.
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        ceiling_vertex_fingerprint(&c),
        before,
        "derive → reload → derive is a fixpoint"
    );
}

/// THE BOUNDARY-RIDGE FULL FIXPOINT (gables): a wall-to-wall ridge over a
/// STEPPED piece — the apexes sit exactly ON the outline, the derived step
/// ring is boolean-op output (the noisy path the EPS claim band exists for).
/// Resolve → commit-style reconstruct (canonicalize the session step list
/// from the derived ceilings) → persist → reload → re-resolve must reproduce
/// the surface identically, and the wall under each ridge end must carry the
/// gable apex on its top. (The apexes are stamped directly — the typed-start
/// flow can't reach the boundary until the end snaps land; this is the
/// durable-input path a snap will drive.)
#[test]
fn boundary_ridge_over_a_step_full_fixpoint_and_gable_walls() {
    use ber_model::ber::structural::CeilingApex;
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).unwrap(); // right of x = 4 at 2.7

    // The wall-to-wall gable ridge over the stepped piece: ends ON the
    // outline walls y = 0 / y = 10 at x = 7.
    let record = c.current_record.as_mut().unwrap();
    for y in [0.0, 10.0] {
        record.data.add_ceiling_apex(
            0,
            CeilingApex {
                point: Point2f64::new(7.0, y),
                z_m: 4.5,
            },
        );
    }
    c.mark_dirty().unwrap();
    // Commit-style canonicalize: compose the apexes into the ceilings (regions-as-
    // truth), derive, then rebuild the session step list from the derived ceilings.
    c.recompose_ceilings(0);
    c.materialize();
    c.reconstruct_ceiling_step_regions_for(0);
    let before = ceiling_vertex_fingerprint(&c);
    assert!(
        before.iter().any(|v| (v.2 - 4.5).abs() < 1e-9),
        "ridge present"
    );
    // The boundary apexes were CLAIMED by the stepped piece (no warn-skip):
    // its slopes rise from the stepped base ≈ 2.7 (the delta round-trips
    // f32), and the session list holds ONE merged area covering the whole
    // stepped half at the step delta.
    assert!(before.iter().any(|v| (v.2 - 2.7).abs() < 1e-6), "eaves");
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "one merged area, not slope faces");
    assert!((session[0].delta_m - 0.3).abs() < 1e-6);
    // The grouping authority links both boundary apexes into ONE ridge.
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let groups = storey.ceilings.apex_groups(&storey.ceiling_apexes);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].apex_idxs.len(), 2, "a gable ridge");
    // The gable walls: the outline walls under the ridge ends (y = 0 and
    // y = 10) carry the apex vertex (7, ·, 4.5) on their top profile.
    let gable_tops = storey
        .walls
        .regions
        .iter()
        .filter(|w| {
            w.polygon
                .exterior
                .iter()
                .any(|v| (v.x - 7.0).abs() < 1e-6 && (v.z - 4.5).abs() < 1e-6)
        })
        .count();
    assert_eq!(gable_tops, 2, "both end walls carry the gable apex");

    // Persist → reload → re-resolve: byte-identical surface (the fixpoint).
    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 2, "apex pair persisted");
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "merged area reconstructed");
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        ceiling_vertex_fingerprint(&c),
        before,
        "resolve → reconstruct → re-resolve is a fixpoint"
    );
}

/// Deleting ONE apex of a ridge degrades it to a fan on the next derive —
/// the group keeps the remaining apex, and the ridge overlay disappears.
#[test]
fn deleting_one_ridge_apex_degrades_to_a_fan() {
    let mut c = ceiling_apex_ready_controller();
    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    c.skip_roof_ridge_length().unwrap();
    c.apply_position_dimension(4.0).unwrap(); // cross
    c.skip_position_dimension().unwrap(); // start: drawn stands
    c.assign_roof_ridge_height(4.2).unwrap();
    c.materialize();

    c.delete_ceiling_apex(0);
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 1, "one apex left");
    let groups = storey.ceilings.apex_groups(&storey.ceiling_apexes);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].apex_idxs.len(), 1, "ridge degraded to a fan");
    assert!(
        c.ceiling_ridge_views().is_empty(),
        "no two-apex overlay left"
    );
    let zs: Vec<f64> = storey
        .ceilings
        .regions
        .iter()
        .flat_map(|r| r.polygon.exterior.iter().map(|v| v.z))
        .collect();
    assert!(
        zs.iter().any(|z| (z - 4.2).abs() < 1e-6),
        "the remaining apex still raises its fan"
    );
}

/// Rejections at every typed stage keep the pending ridge open for a
/// retype, each stage rejecting only what is KNOWABLE at it (the
/// validation/stage table): the LENGTH stage rejects a zero-ish value and
/// one beyond EVERY ceiling area's bbox (the optimistic bound, with the
/// permitted bound in the message); a length that fits the bbox but not the
/// actual run is accepted at Length and rejected at the LAST OPEN location
/// stage (here the start), where the geometry becomes fully determined —
/// with the permitted run, the rejection attached to the typed position;
/// the HEIGHT stage rejects only the height value itself. Off-footprint and
/// non-line strokes never go pending at all, and a tap while a ridge is
/// pending does NOT start a point flow.
#[test]
fn ceiling_ridge_rejections_keep_the_pending_and_bad_strokes_never_pend() {
    let mut c = ceiling_apex_ready_controller();
    // A stroke outside the footprint is rejected up front.
    c.apply_roof_ridge_stroke_event(&[(15.0, 2.0), (15.0, 5.0), (15.0, 8.0)]);
    assert!(!c.ceiling_ridge_pending(), "off-footprint stroke rejected");

    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    assert!(c.ceiling_ridge_length_pending(), "length is question 1");
    // A tap while the ridge is pending must not spawn a roof point.
    c.ceiling_point_tap(7.0, 5.0);
    assert!(
        !c.ceiling_point_pending(),
        "tap suppressed while ridge pends"
    );

    // Length rejections: zero-ish, then beyond every area's bbox (the
    // 10×10 footprint's one flat region runs 10 m along the ridge axis) —
    // with the permitted bound in the message.
    let err = c.assign_roof_ridge_length(0.0).unwrap_err();
    assert!(err.contains("more than a centimetre"), "got: {err}");
    let err = c.assign_roof_ridge_length(11.0).unwrap_err();
    assert!(err.contains("No ceiling area is that long"), "got: {err}");
    assert!(err.contains("under 10.00 m"), "got: {err}");
    assert!(
        c.ceiling_ridge_length_pending(),
        "pending intact on rejection"
    );

    // 9 m fits the bbox → ACCEPTED at Length (the bound is deliberately
    // optimistic — no location is known yet)…
    c.assign_roof_ridge_length(9.0)
        .expect("a bbox-fitting length passes the Length stage");
    c.apply_position_dimension(4.0).unwrap(); // cross → x = 4
                                              // …and rejected at the LAST OPEN stage: a typed start of 2 m (from
                                              // y = 0) pushes the far end to y = 11, past the wall — the finalize
                                              // check rejects the START entry with the full wall-to-wall run, and
                                              // the prompt stays open.
    let err = c.apply_position_dimension(2.0).unwrap_err();
    assert!(err.contains("out of its ceiling area"), "got: {err}");
    assert!(err.contains("10.00 m wall to wall"), "got: {err}");
    assert!(
        c.ceiling_ridge_position_pending(),
        "start prompt stays open on the finalize rejection"
    );
    // A start the length fits from (y = 0.5: the extent ends 0.5 m clear
    // of the far wall) lands → height.
    c.apply_position_dimension(0.5)
        .expect("fitting start lands");
    assert!(c.ceiling_ridge_height_pending());

    let err = c.assign_roof_ridge_height(2.405).unwrap_err();
    assert!(err.contains("within a centimetre"), "got: {err}");
    assert!(
        c.ceiling_ridge_height_pending(),
        "pending intact on rejection"
    );
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(storey.ceiling_apexes.is_empty(), "nothing committed");

    c.assign_roof_ridge_height(4.2).expect("valid height lands");
    assert!(!c.ceiling_ridge_pending());
}

/// A finalised notched-C storey (a 10×4 bar with a slot cut from the right,
/// y ∈ [1, 3] reaching in to x = 3) with its default ceiling height set and
/// roof-point mode entered — the foldover-fence fixture.
fn ceiling_ready_notched_controller() -> AppController {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    c.document.floor_plan = FloorPlan::from_clean_ring(&[
        p(0., 0.),
        p(10., 0.),
        p(10., 1.),
        p(3., 1.),
        p(3., 3.),
        p(10., 3.),
        p(10., 4.),
        p(0., 4.),
    ])
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
    c.set_default_ceiling_height(2.4).expect("height applies");
    c.set_roof_point_mode(true);
    assert!(c.ceiling_point_mode());
    c
}

/// THE HUB-SWITCH FENCE AT FINALIZE (M1's accepted-early/rejected-late
/// regression, controller leg): a ridge along the notched C's top arm
/// passes every span/claim predicate but folds over the slot (4 hub
/// switches) — only the Height stage used to catch it, where no typed
/// height can fix it. The finalize check at the LAST open location stage
/// must reject it instead, with the rejection landing on the typed start
/// and the prompt left open (the no-late-rejection shape of the existing
/// finalize-rejection tests).
#[test]
fn ceiling_ridge_foldover_is_rejected_at_finalize_not_the_height_stage() {
    let mut c = ceiling_ready_notched_controller();
    // Horizontal ridge through the top arm: cross y ≈ 3.45 (no snap — the
    // y = 3 wall is 0.45 away), extent x ∈ [1, 9] (ends 1 m clear of the
    // perpendicular walls — no end snaps).
    c.apply_roof_ridge_stroke_event(&[(1.0, 3.43), (5.0, 3.47), (9.0, 3.45)]);
    assert!(c.ceiling_ridge_length_pending(), "length is question 1");
    c.assign_roof_ridge_length(8.0)
        .expect("8 m fits the C's 10 m bbox — the optimistic bound passes");
    // Cross: 0.5 m from the nearest parallel wall (y = 3) → y = 3.5.
    c.apply_position_dimension(0.5).expect("cross applies");
    // Start (the LAST open location stage): 1 m from x = 0 → lo = 1. The
    // geometry is now fully determined and the foldover fence fires HERE —
    // not at the Height stage.
    let err = c.apply_position_dimension(1.0).unwrap_err();
    assert!(
        err.contains("too irregular for a single ridge"),
        "the Height stage's stable foldover message, surfaced at finalize: {err}"
    );
    assert!(
        c.ceiling_ridge_position_pending(),
        "start prompt stays open on the finalize rejection"
    );
    assert!(
        !c.ceiling_ridge_height_pending(),
        "the un-fixable Height stage is never reached"
    );
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(storey.ceiling_apexes.is_empty(), "nothing committed");
}

/// SKIP-PATH FINALIZE REJECTIONS SURFACE (L1): a Skip at the last open
/// location stage runs the same finalize check a typed value does, and its
/// rejection is RETURNED (for the inline error), not just logged — the
/// prompt stays open.
#[test]
fn ceiling_ridge_skip_at_the_last_stage_surfaces_the_finalize_rejection() {
    let mut c = ceiling_apex_ready_controller();
    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    // 9 m from the drawn lo (y = 2) will overrun the 10 m footprint — but
    // the Length stage's bound is optimistic, so it passes here.
    c.assign_roof_ridge_length(9.0).expect("bbox bound passes");
    c.skip_position_dimension()
        .expect("cross skip: drawn x stands, start question remains");
    // Start skip: the drawn lo stands → finalize → extent [2, 11] overruns.
    let err = c.skip_position_dimension().unwrap_err();
    assert!(
        err.contains("out of its ceiling area") && err.contains("10.00 m wall to wall"),
        "the finalize rejection surfaces on the skip path: {err}"
    );
    assert!(
        c.ceiling_ridge_position_pending(),
        "start prompt stays open on the skipped finalize rejection"
    );
}

/// THE STAGE-ORDER TABLE (design §3, normative): Length is question 1 and
/// is always asked; the Cross and Start questions follow, each SKIPPED when
/// its degree of freedom snapped at stroke time; the finalize placement
/// check runs at the last open stage; then Height. One row per snap
/// combination, driven by where the stroke lands in the L footprint (the
/// interior wall x = 6 and the horizontal wall y = 4 are the snap targets).
#[test]
fn ceiling_ridge_stage_order_table() {
    // The pending ridge's stage as a short label. The two position
    // questions are told apart by the open prompt's axis — the table uses
    // VERTICAL strokes only, so X is the cross question, Y the start.
    fn stage(c: &AppController) -> &'static str {
        if c.ceiling_ridge_length_pending() {
            return "len";
        }
        if c.ceiling_ridge_height_pending() {
            return "height";
        }
        if c.ceiling_ridge_position_pending() {
            let ui = c
                .position_prompt_for_ui()
                .expect("position stage has a prompt");
            return match ui.axis {
                ParameterAxis::X => "cross",
                ParameterAxis::Y => "start",
            };
        }
        "none"
    }
    let rows: [(&str, [(f32, f32); 2], &[&str]); 5] = [
        (
            "no snaps",
            [(2.78, 1.0), (2.8, 3.0)],
            &["len", "cross", "start", "height"],
        ),
        (
            "cross snapped (x ≈ 5.9 → 6)",
            [(5.88, 0.5), (5.9, 3.5)],
            &["len", "start", "height"],
        ),
        (
            "start snapped (y ≈ 4.1 → 4)",
            [(2.78, 4.1), (2.8, 8.5)],
            &["len", "cross", "height"],
        ),
        (
            // The END snap fixes the position DOF: Start is skipped, but
            // the Length question still opens (one end free).
            "end snapped (hi ≈ 9.9 → 10, a bbox extreme)",
            [(2.78, 5.0), (2.8, 9.9)],
            &["len", "cross", "height"],
        ),
        (
            // BOTH ends snapped: the length is exact by construction —
            // Length AND Start are skipped.
            "both ends snapped (wall to wall)",
            [(2.78, 0.1), (2.8, 9.9)],
            &["cross", "height"],
        ),
    ];
    for (name, stroke, want) in rows {
        let mut c = ceiling_ready_l_controller();
        c.set_roof_point_mode(true);
        c.apply_roof_ridge_stroke_event(&stroke);
        let mut seq: Vec<&str> = Vec::new();
        let mut guard = 0;
        while c.ceiling_ridge_pending() && guard < 6 {
            let s = stage(&c);
            seq.push(s);
            match s {
                "len" => c
                    .skip_roof_ridge_length()
                    .unwrap_or_else(|e| panic!("{name}: length skip rejected: {e}")),
                "cross" | "start" => c
                    .skip_position_dimension()
                    .unwrap_or_else(|e| panic!("{name}: position skip rejected: {e}")),
                "height" => break,
                other => panic!("{name}: unexpected stage {other}"),
            }
            guard += 1;
        }
        assert_eq!(&seq, want, "{name}");
    }
}

/// THE WALL-TO-WALL DRAW, end to end (the S4 gate): a stroke whose BOTH
/// extent ends land within snap tolerance of opposite outline walls — bbox
/// extremes INCLUDED, the deliberate difference from the step-line snap —
/// snaps both ends, which makes the length exact by construction: the
/// LENGTH question is skipped, the cross question opens (with the END tag
/// showing — its question, the Length, has been passed), the START question
/// is skipped (the position DOF is fixed), finalize allows the boundary
/// touch, and one typed height lands the GABLE ridge: apexes exactly ON
/// both walls, the end walls carrying the triangular gable top.
#[test]
fn ceiling_ridge_wall_to_wall_draw_succeeds_end_to_end() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    // Vertical stroke at x ≈ 2.8 (no parallel wall within 0.2), running
    // y ≈ 0.1 → 9.9: lo snaps to the bbox-extreme wall y = 0, hi to y = 10.
    c.apply_roof_ridge_stroke_event(&[(2.78, 0.1), (2.8, 9.9)]);
    assert!(
        !c.ceiling_ridge_length_pending(),
        "both ends snapped → the length is exact by construction, no question"
    );
    let ui = c
        .position_prompt_for_ui()
        .expect("the cross question opens");
    assert_eq!(ui.axis, ParameterAxis::X);
    let ((_, ay), (_, by)) = c.pending_roof_ridge_line().unwrap();
    assert!(
        ay.abs() < 1e-6 && (by - 10.0).abs() < 1e-6,
        "extent wall to wall, exact"
    );
    // The END tag shows at the cross stage (its question — the Length —
    // has been passed; end outranks the riding start snap), at the HI end.
    let tag = c
        .ceiling_ridge_snap_tag()
        .expect("end tag at the cross stage");
    assert!((tag.pos.1 - 10.0).abs() < 1e-4, "tag at the HI endpoint");
    assert!((tag.from.1 - 9.9).abs() < 0.05, "identity = the drawn end");
    assert!(tag.options.is_empty() && tag.skippable, "Don't snap only");

    c.skip_position_dimension().unwrap(); // cross: drawn x ≈ 2.8 stands
    assert!(
        c.ceiling_ridge_height_pending(),
        "start skipped (either end snap fixes the position) → height"
    );
    c.assign_roof_ridge_height(4.0)
        .expect("the wall-to-wall gable ridge assigns");
    assert!(!c.ceiling_ridge_pending(), "flow complete");
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 2, "both apexes durable");
    for (apex, want_y) in storey.ceiling_apexes.iter().zip([0.0, 10.0]) {
        assert!(
            (apex.point.y - want_y).abs() < 1e-9,
            "apex exactly ON the wall (got {:?})",
            apex.point
        );
    }
    // The end walls carry the triangular gable top (the apex vertex on the
    // wall's top profile) — straight out of WallRegions::regenerate.
    let gable_walls = storey
        .walls
        .regions
        .iter()
        .filter(|w| {
            w.polygon.exterior.iter().any(|v| {
                (v.z - 4.0).abs() < 1e-6 && (v.y.abs() < 1e-6 || (v.y - 10.0).abs() < 1e-6)
            })
        })
        .count();
    assert_eq!(gable_walls, 2, "both end walls carry the gable apex");
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "a gable over the default base needs no session step state"
    );
}

/// A ridge drawn ALONG an OUTLINE wall (here the L's right wall x = 6, from the
/// notch corner up) is now ACCEPTED as a mono-pitch: cross + start snap put it
/// exactly on the wall, its midpoint sits ON the outline boundary, and instead
/// of the old "along a wall makes no roof" rejection it raises that boundary
/// wall and slopes the single ceiling area it bounds (the user's "on the image
/// boundary … just raise the boundary wall"). A single claimant means NO area
/// question — the finalize advances straight to the height.
#[test]
fn ceiling_ridge_along_an_outline_wall_raises_it_as_a_mono_pitch() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    // x ≈ 5.9 → snaps 6; lo ≈ 4.1 → snaps 4: the ridge runs up the wall
    // x = 6 from the corner (6, 4). hi = 8.5 is beyond every snap.
    c.apply_roof_ridge_stroke_event(&[(5.88, 4.1), (5.9, 8.5)]);
    assert!(c.ceiling_ridge_length_pending(), "length is question 1");
    let ((ax, ay), _) = c.pending_roof_ridge_line().unwrap();
    assert!(
        (ax - 6.0).abs() < 1e-6 && (ay - 4.0).abs() < 1e-6,
        "both snaps landed"
    );
    // The finalize now ACCEPTS the boundary ridge and advances to the height.
    c.assign_roof_ridge_length(2.0)
        .expect("a ridge along the outline wall is a valid mono-pitch");
    assert!(
        c.ceiling_ridge_height_pending(),
        "advanced to the height question"
    );
    // One claiming area → no area choice; the height applies directly.
    c.assign_roof_ridge_height(3.0).expect("height applies");
    assert!(!c.ceiling_ridge_pending(), "ridge finalized");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 2, "both apexes durable");
    for apex in &storey.ceiling_apexes {
        assert!(
            (apex.point.x - 6.0).abs() < 1e-6,
            "apex on the outline wall x = 6 (got {:?})",
            apex.point
        );
    }
}

/// THE CENTRE BUTTON (S5): both ridge location prompts offer it
/// (`offers_centre`), and clicking it applies the computed
/// centre-it-within-its-ceiling-area value through the SAME advance path as
/// a typed distance — the cross centres to the area's midline, the start
/// centres the typed length along the run, the stage machine advances
/// exactly as for a typed value, and the assigned apexes land at the
/// centred coordinates.
#[test]
fn centre_button_centres_cross_and_start_within_the_area() {
    let mut c = ceiling_apex_ready_controller();
    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    c.assign_roof_ridge_length(4.0).expect("length applies");

    // The cross question offers Centre; clicking it lands x = 5 (the 10×10
    // area's midline) and advances to the start question.
    let ui = c.position_prompt_for_ui().expect("cross prompt");
    assert_eq!(ui.axis, ParameterAxis::X);
    assert!(ui.offers_centre, "the ridge cross stage offers Centre");
    c.centre_roof_ridge_position().expect("cross centres");
    let ((ax, _), (bx, _)) = c.pending_roof_ridge_line().unwrap();
    assert!(
        (ax - 5.0).abs() < 1e-6 && (bx - 5.0).abs() < 1e-6,
        "cross centred on the area midline (got {ax})"
    );

    // The start question offers it too; the 4 m length centres to [3, 7].
    let ui = c.position_prompt_for_ui().expect("start prompt follows");
    assert_eq!(ui.axis, ParameterAxis::Y);
    assert!(ui.offers_centre, "the ridge start stage offers Centre");
    c.centre_roof_ridge_position().expect("start centres");
    assert!(
        c.ceiling_ridge_height_pending(),
        "centre advances exactly like a typed value — finalize passed"
    );
    let ((_, ay), (_, by)) = c.pending_roof_ridge_line().unwrap();
    assert!(
        (ay - 3.0).abs() < 1e-6 && (by - 7.0).abs() < 1e-6,
        "extent centred along the run (got {ay}..{by})"
    );

    c.assign_roof_ridge_height(4.2).expect("ridge assigns");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    for (apex, want_y) in storey.ceiling_apexes.iter().zip([3.0, 7.0]) {
        assert!((apex.point.x - 5.0).abs() < 1e-6);
        assert!((apex.point.y - want_y).abs() < 1e-6);
    }
}

/// Centre ROUTES THROUGH VALIDATION like a typed value: a Centre whose
/// length can't fit the run through the midpoint is rejected with the
/// stable exceeds-area message (+ the permitted run) and the prompt stays
/// open; non-ridge position prompts never offer Centre, and the handler is
/// a no-op when no ridge location stage is up.
#[test]
fn centre_rejection_lands_on_the_open_prompt_and_only_ridge_stages_offer_it() {
    // The roof-point tap flow's prompt does NOT offer Centre, and the
    // ridge-centre handler is a no-op on it.
    let mut c = ceiling_apex_ready_controller();
    c.ceiling_point_tap(3.8, 5.2);
    let ui = c.position_prompt_for_ui().expect("tap flow's X prompt");
    assert!(!ui.offers_centre, "the tap flow has no Centre");
    c.centre_roof_ridge_position().expect("no-op");
    assert!(
        c.ceiling_point_pending() && !c.ceiling_ridge_pending(),
        "tap flow untouched"
    );
    c.cancel_pending_roof_point();

    // L footprint, vertical ridge in the RIGHT band (run y ∈ [0, 4]): the
    // drawn 5 m length passes the optimistic Length bound (the bbox runs
    // 10 m along Y) but exceeds the 4 m run through the midpoint — the
    // Centre click is rejected exactly like a typed start.
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    c.apply_roof_ridge_stroke_event(&[(7.98, 0.5), (8.02, 5.5)]);
    c.skip_roof_ridge_length().unwrap(); // drawn 5 m stands
    c.skip_position_dimension().unwrap(); // cross: drawn x ≈ 8 stands
    let ui = c.position_prompt_for_ui().expect("start prompt");
    assert_eq!(ui.axis, ParameterAxis::Y);
    let err = c.centre_roof_ridge_position().unwrap_err();
    assert!(
        err.contains("out of its ceiling area") && err.contains("4.00 m"),
        "the typed-value rejection, on the Centre click: {err}"
    );
    assert!(
        c.ceiling_ridge_position_pending(),
        "prompt stays open on the rejection"
    );
    c.cancel_pending_roof_ridge();

    // A length that fits the band centres within it: 3 m → lo = 0.5.
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    c.apply_roof_ridge_stroke_event(&[(7.98, 0.5), (8.02, 5.5)]);
    c.assign_roof_ridge_length(3.0).unwrap();
    c.skip_position_dimension().unwrap(); // cross: drawn x ≈ 8 stands
    c.centre_roof_ridge_position()
        .expect("fitting length centres");
    assert!(c.ceiling_ridge_height_pending());
    let ((_, ay), (_, by)) = c.pending_roof_ridge_line().unwrap();
    assert!(
        (ay - 0.5).abs() < 1e-6 && (by - 3.5).abs() < 1e-6,
        "3 m centred in the 4 m band (got {ay}..{by})"
    );
}

// ── roof RECT (flat-topped hip / pavilion) ──────────────────────────────────

/// A hand-wavy CLOSED rectangle around (2,2)..(6,5) — routes to the rect
/// flow (ends meet + encloses area, the carve's closure classification).
fn rect_stroke() -> Vec<(f32, f32)> {
    vec![
        (2.0, 2.0),
        (4.0, 1.95),
        (6.0, 2.0),
        (6.05, 3.5),
        (6.0, 5.0),
        (4.0, 5.05),
        (2.0, 5.0),
        (1.95, 3.5),
        (2.0, 2.1),
    ]
}

/// THE RECT MULTI-STAGE FLOW incl. CENTRE (design §5): a CLOSED stroke in
/// roof-point mode routes to the rect flow — LenX (prefilled drawn, typed)
/// → LenY (typed) → PosX (typed distance, Centre offered) → PosY (the
/// Centre button) → finalize → Height — and the commit stamps FOUR corner
/// apexes in ONE group: the pavilion, with its flat top at the entered
/// height and the committed-rect overlay labelled.
#[test]
fn ceiling_rect_closed_stroke_flow_with_centre_forms_a_pavilion() {
    let mut c = ceiling_apex_ready_controller();
    c.apply_roof_ridge_stroke_event(&rect_stroke());
    assert!(
        c.ceiling_rect_pending() && !c.ceiling_ridge_pending(),
        "a closed stroke routes to the RECT flow"
    );
    assert_eq!(c.ceiling_rect_length_pending(), Some(ParameterAxis::X));
    let prefill = c.pending_roof_rect_length().expect("drawn width");
    assert!((prefill - 4.1).abs() < 0.01, "got {prefill}");
    c.assign_roof_rect_length(4.0).expect("width applies");

    assert_eq!(c.ceiling_rect_length_pending(), Some(ParameterAxis::Y));
    let prefill = c.pending_roof_rect_length().expect("drawn depth");
    assert!((prefill - 3.1).abs() < 0.01, "LenY refills with the depth");
    c.assign_roof_rect_length(2.0).expect("depth applies");

    // PosX: the shared position panel, kind Line, Centre offered; the area
    // wall is the rect's LEFT side. Type 3 m from x = 0 → lo.x = 3.
    let ui = c.position_prompt_for_ui().expect("PosX");
    assert_eq!(ui.axis, ParameterAxis::X);
    assert_eq!(ui.kind, PositionPromptKind::Line);
    assert!(ui.offers_centre, "the rect location stages offer Centre");
    assert_eq!(
        ui.area_wall.wall_a.0, ui.area_wall.wall_b.0,
        "the measured object is the rect's LO side"
    );
    c.apply_position_dimension(3.0).expect("PosX applies");

    // PosY: Centre → the 2 m depth centres at lo.y = 4 and finalize passes.
    let ui = c.position_prompt_for_ui().expect("PosY");
    assert_eq!(ui.axis, ParameterAxis::Y);
    assert!(ui.offers_centre);
    c.centre_roof_position()
        .expect("centre applies like a typed value");
    assert!(c.ceiling_rect_height_pending(), "finalize passed → height");
    let outline = c.pending_roof_rect_outline().expect("dashed overlay");
    assert!((outline[0].0 - 3.0).abs() < 1e-6 && (outline[0].1 - 4.0).abs() < 1e-5);
    assert!((outline[2].0 - 7.0).abs() < 1e-6 && (outline[2].1 - 6.0).abs() < 1e-5);

    c.assign_roof_rect_height(4.2).expect("rect assigns");
    assert!(!c.ceiling_rect_pending(), "flow complete");
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 4, "four corner apexes durable");
    let groups = storey.ceilings.apex_groups(&storey.ceiling_apexes);
    assert_eq!(groups.len(), 1, "ONE group");
    assert_eq!(groups[0].apex_idxs.len(), 4, "…holding all four corners");
    assert!(
        storey
            .ceilings
            .regions
            .iter()
            .any(|r| r.flat_z().is_some_and(|z| (z - 4.2).abs() < 1e-6)
                && r.polygon.exterior.len() == 4),
        "the flat top at the entered height"
    );
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "a pavilion over the default base needs NO session step state"
    );
    let views = c.ceiling_rect_views();
    assert_eq!(views.len(), 1, "one committed rect overlay");
    assert_eq!(views[0].1, "⌃ 4.20 m");
    assert!(c.ceiling_ridge_views().is_empty(), "not a two-apex ridge");
}

/// Rect rejections keep the open stage for a retype (the ridge's
/// no-late-rejection shape): the LENGTH stages reject zero-ish sides and
/// ones beyond every area's bbox; the finalize check rejects a typed
/// POSITION that pushes the rect out (with the permitted run); the HEIGHT
/// stage rejects a near-base value. Off-footprint closed strokes never
/// pend; taps and new strokes are suppressed while a rect is pending;
/// Cancel recovers.
#[test]
fn ceiling_rect_rejections_keep_stages_open_and_cancel_recovers() {
    let mut c = ceiling_apex_ready_controller();
    // Off the footprint (beyond the snap tolerance): never pends.
    c.apply_roof_ridge_stroke_event(&[
        (12.0, 2.0),
        (14.0, 2.0),
        (14.0, 4.0),
        (12.0, 4.0),
        (12.0, 2.1),
    ]);
    assert!(!c.ceiling_rect_pending(), "off-footprint rect rejected");

    c.apply_roof_ridge_stroke_event(&rect_stroke());
    assert!(c.ceiling_rect_pending());
    // A tap and a new stroke while pending are suppressed.
    c.ceiling_point_tap(8.0, 8.0);
    assert!(
        !c.ceiling_point_pending(),
        "tap suppressed while rect pends"
    );
    c.apply_roof_ridge_stroke_event(&[(7.0, 2.0), (7.0, 8.0)]);
    assert!(
        !c.ceiling_ridge_pending(),
        "stroke suppressed while rect pends"
    );

    // Length rejections: zero-ish, then beyond the 10×10 bbox.
    let err = c.assign_roof_rect_length(0.0).unwrap_err();
    assert!(err.contains("more than a centimetre"), "got: {err}");
    let err = c.assign_roof_rect_length(11.0).unwrap_err();
    assert!(err.contains("No ceiling area is that long"), "got: {err}");
    assert_eq!(
        c.ceiling_rect_length_pending(),
        Some(ParameterAxis::X),
        "stage intact on rejection"
    );
    c.skip_roof_rect_length().unwrap(); // width: drawn 4.1 stands
    c.skip_roof_rect_length().unwrap(); // depth: drawn 3.1 stands
    c.apply_position_dimension(3.0).expect("PosX applies"); // lo.x = 3

    // PosY: 8 m from y = 0 pushes the rect to y ∈ [8, 11.1] — out the far
    // wall. The finalize rejection lands on the typed value with the
    // permitted run; the prompt stays open.
    let err = c.apply_position_dimension(8.0).unwrap_err();
    assert!(
        err.contains("flat top out of its ceiling area") && err.contains("10.00 m"),
        "got: {err}"
    );
    assert!(c.ceiling_rect_position_pending(), "PosY stays open");
    c.apply_position_dimension(4.0)
        .expect("a fitting position lands");
    assert!(c.ceiling_rect_height_pending());

    // Height rejections keep the stage; nothing committed.
    let err = c.assign_roof_rect_height(2.405).unwrap_err();
    assert!(err.contains("centimetre"), "got: {err}");
    assert!(c.ceiling_rect_height_pending());
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(storey.ceiling_apexes.is_empty(), "nothing committed");
    c.cancel_pending_roof_rect();
    assert!(!c.ceiling_rect_pending(), "cancel recovers");
}

/// PER-STAGE LENGTH ATTRIBUTION (L2): each rect length stage validates only
/// ITS axis's typed value — a bad DRAWN depth must not reject a good typed
/// width at LenX with the depth's message. A ceiling step at y = 4 splits
/// the 10×10 roof into 10×4 and 10×6 areas (Y bound 6, X bound 10); the
/// rect is drawn ≈ 4.1 wide × 7.1 deep, so the drawn depth already exceeds
/// every area's Y extent. LenX accepts a fitting typed width; LenY rejects
/// the over-deep value at ITS stage (with the Y bound) and accepts a
/// fitting one.
#[test]
fn ceiling_rect_length_stages_validate_only_their_own_axis() {
    let mut c = ceiling_ready_controller();
    // Step line along y = 4 (horizontal stroke → axis Y), raised above.
    c.apply_ceiling_step_stroke_event(&[(0.5, 3.78), (5.0, 3.82), (9.5, 3.8)]);
    c.ceiling_step_tap(5.0, 7.0); // above the line → Greater side
    c.apply_position_dimension(4.0)
        .expect("line pinned at y = 4");
    c.assign_ceiling_step_delta(0.3).unwrap();
    c.set_roof_point_mode(true);
    // A closed rect ≈ (2, 1)–(6, 8): drawn depth ≈ 7.1 > the 6 m Y bound.
    c.apply_roof_ridge_stroke_event(&[
        (2.0, 1.0),
        (4.0, 0.95),
        (6.0, 1.0),
        (6.05, 4.5),
        (6.0, 8.0),
        (4.0, 8.05),
        (2.0, 8.0),
        (1.95, 4.5),
        (2.0, 1.1),
    ]);
    assert_eq!(c.ceiling_rect_length_pending(), Some(ParameterAxis::X));
    // The fix: a good typed WIDTH passes LenX even though the drawn depth
    // is bad (it used to be validated alongside and reject with Y's bound).
    c.assign_roof_rect_length(4.0)
        .expect("LenX validates only the typed width");
    assert_eq!(c.ceiling_rect_length_pending(), Some(ParameterAxis::Y));
    // The bad depth rejects at ITS OWN stage, with the Y-axis bound.
    let err = c.assign_roof_rect_length(7.0).unwrap_err();
    assert!(
        err.contains("No ceiling area is that long") && err.contains("under 6.00 m"),
        "got: {err}"
    );
    assert_eq!(
        c.ceiling_rect_length_pending(),
        Some(ParameterAxis::Y),
        "LenY stays open on rejection"
    );
    c.assign_roof_rect_length(3.0)
        .expect("a fitting depth applies");
    assert!(
        c.ceiling_rect_position_pending(),
        "on to the position stages"
    );
}

/// THE RECT FIXPOINT over the DEFAULT base (design §5's required test):
/// resolve → commit-style canonicalize → persist → reload → re-resolve
/// reproduces the pavilion byte for byte, from the four durable apexes
/// with no session state.
#[test]
fn ceiling_rect_survives_reload_and_rederive_fixpoint() {
    let mut c = ceiling_apex_ready_controller();
    c.apply_roof_ridge_stroke_event(&rect_stroke());
    c.skip_roof_rect_length().unwrap();
    c.skip_roof_rect_length().unwrap();
    c.skip_position_dimension().unwrap(); // PosX: drawn stands
    c.skip_position_dimension().unwrap(); // PosY: drawn stands → finalize → height
    assert!(c.ceiling_rect_height_pending());
    c.assign_roof_rect_height(4.2).unwrap();
    c.materialize();
    let before = ceiling_vertex_fingerprint(&c);

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 4, "corner apexes persisted");
    let groups = storey.ceilings.apex_groups(&storey.ceiling_apexes);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].apex_idxs.len(), 4, "still one rect group");
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "no session state for a default-base pavilion"
    );
    assert_eq!(c.ceiling_rect_views().len(), 1, "overlay re-derives");

    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        ceiling_vertex_fingerprint(&c),
        before,
        "derive → reload → derive is a fixpoint"
    );
}

/// THE RECT FIXPOINT over a STEPPED base (design §5's second required
/// test — the boolean-noise path the EPS claim band and the apex_groups
/// all-apex-top amendment exist for): a pavilion over a raised ceiling
/// piece canonicalizes to ONE merged step area (top included — no hole),
/// persists, reloads, and re-resolves identically.
#[test]
fn ceiling_rect_over_a_step_full_fixpoint() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).unwrap(); // right of x = 4 at 2.7
    c.set_roof_point_mode(true);
    assert!(c.ceiling_point_mode());
    // A closed rect over the stepped piece (x ∈ [4, 10]).
    c.apply_roof_ridge_stroke_event(&[
        (5.0, 2.0),
        (7.0, 1.95),
        (9.0, 2.0),
        (9.05, 4.0),
        (9.0, 6.0),
        (7.0, 6.05),
        (5.0, 6.0),
        (4.95, 4.0),
        (5.0, 2.1),
    ]);
    assert!(c.ceiling_rect_pending());
    c.skip_roof_rect_length().unwrap();
    c.skip_roof_rect_length().unwrap();
    c.skip_position_dimension().unwrap();
    c.skip_position_dimension().unwrap();
    assert!(c.ceiling_rect_height_pending(), "drawn rect finalizes");
    c.assign_roof_rect_height(4.5).unwrap();
    c.materialize();
    let before = ceiling_vertex_fingerprint(&c);
    assert!(
        before.iter().any(|v| (v.2 - 4.5).abs() < 1e-9),
        "flat top present"
    );
    assert!(
        before.iter().any(|v| (v.2 - 2.7).abs() < 1e-6),
        "eaves rise from the STEPPED base"
    );
    // The canonical session list is the ONE drawn step area (the stepped half),
    // clipped to the outline — the apex composes on top without changing it.
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "one step area, the drawn half");
    assert!((session[0].delta_m - 0.3).abs() < 1e-6);
    // It is mirrored to the durable field for reload.
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .ceiling_steps
            .len(),
        1,
        "durable step list mirrors the session"
    );
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let groups = storey.ceilings.apex_groups(&storey.ceiling_apexes);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].apex_idxs.len(), 4);

    // Persist → reload → re-resolve: byte-identical (the fixpoint).
    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 4, "apexes persisted");
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "merged area reconstructed");
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        ceiling_vertex_fingerprint(&c),
        before,
        "resolve → reconstruct → re-resolve is a fixpoint"
    );
}

/// THE COPLANAR-FLUSH RISER MERGE FIXPOINT (live report, 2026-06): the
/// L/U-shaped storey whose ceiling-step rect CONTINUES two perimeter wall
/// planes resolves with the flush risers merged into their walls (11
/// regions — 10 perimeter, two of them L-profiled, plus the single
/// surviving riser), persists, reloads, and re-resolves BYTE-IDENTICALLY:
/// the merged polygons re-derive exactly and identity rides the amend.
#[test]
fn flush_riser_merge_survives_reload_and_rederive_fixpoint() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    c.document.floor_plan = FloorPlan::from_clean_ring(&[
        p(5.2184, 6.5205),
        p(5.2184, 8.5205),
        p(8.2184, 8.5205),
        p(8.2184, 2.5205),
        p(4.2184, 2.5205),
        p(4.2184, 1.5205),
        p(2.2184, 1.5205),
        p(2.2184, 5.5205),
        p(3.2184, 5.5205),
        p(3.2184, 6.5205),
    ])
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
    c.set_default_ceiling_height(2.4).expect("height applies");
    // The assessor's carved step rect (+1.0 over the 2.4 default), exactly
    // as the carve flow stores it.
    c.ceiling_step_regions.insert(
        0,
        vec![CeilingStepRegion {
            polygon: vec![
                p(3.2184, 6.5205),
                p(3.2184, 2.5205),
                p(5.2184, 2.5205),
                p(5.2184, 6.5205),
            ],
            delta_m: 1.0,
        }],
    );
    c.mark_dirty().unwrap();
    c.recompose_ceilings(0); // regions-as-truth: compose the step into the ceilings
    c.reconstruct_ceiling_step_regions_for(0); // mirror to the durable field
    c.materialize();
    let walls_fingerprint = |c: &AppController| {
        c.current_record.as_ref().unwrap().data.storeys[0]
            .walls
            .regions
            .iter()
            .map(|r| format!("{:?}", r.polygon))
            .collect::<Vec<_>>()
            .join("|")
    };
    let before = walls_fingerprint(&c);
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .walls
            .regions
            .len(),
        11,
        "10 perimeter walls (two merged with their flush risers) + 1 riser"
    );

    // Persist → reload → re-resolve: byte-identical walls (the fixpoint).
    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "step area reconstructed on reload");
    assert!((session[0].delta_m - 1.0).abs() < 1e-6);
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        walls_fingerprint(&c),
        before,
        "resolve → persist → reload → re-resolve is a fixpoint over the \
         merged walls"
    );
}

/// Risk #4's wall half — the rect TOUCHING ONE WALL (mono-gable side): the
/// four corner apexes with one side ON the outline produce a wall that
/// rises to the flat-top height under that side (the gable profile falls
/// out of WallRegions::regenerate, like the boundary ridge), the overlay
/// shows the committed rect, and a no-edit re-derive is a fixpoint. (The
/// apexes are stamped directly — per-side snapping is deferred, so the
/// drawn flow can't land EXACTLY on the wall yet.)
#[test]
fn ceiling_rect_touching_one_wall_makes_a_mono_gable_wall() {
    use ber_model::ber::structural::CeilingApex;
    let mut c = ceiling_apex_ready_controller();
    let record = c.current_record.as_mut().unwrap();
    for (x, y) in [(3.0, 0.0), (7.0, 0.0), (7.0, 4.0), (3.0, 4.0)] {
        record.data.add_ceiling_apex(
            0,
            CeilingApex {
                point: Point2f64::new(x, y),
                z_m: 4.5,
            },
        );
    }
    c.mark_dirty().unwrap();
    c.recompose_ceilings(0); // regions-as-truth: compose the apexes into the ceilings
    c.materialize();
    c.reconstruct_ceiling_step_regions_for(0);
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let groups = storey.ceilings.apex_groups(&storey.ceiling_apexes);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].apex_idxs.len(), 4, "one rect group");
    // The wall under the touching side (y = 0) carries the on-wall corners
    // at the flat-top height on its top profile — the mono-gable.
    let gable_walls = storey
        .walls
        .regions
        .iter()
        .filter(|w| {
            w.polygon
                .exterior
                .iter()
                .any(|v| (v.z - 4.5).abs() < 1e-6 && v.y.abs() < 1e-6)
        })
        .count();
    assert!(
        gable_walls >= 1,
        "the wall under the touching side rises to the flat top"
    );
    assert_eq!(c.ceiling_rect_views().len(), 1, "committed overlay");
    let before = ceiling_vertex_fingerprint(&c);
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(ceiling_vertex_fingerprint(&c), before, "re-derive fixpoint");
}

/// A snapped HI end with the start un-snapped: the Length question still
/// opens (prefilled with the snapped-end run, reported ANCHORED) and a
/// typed length RE-ANCHORS at the wall — `lo = hi − len`, the far end stays
/// exactly on the snapped coordinate — which fixes the position DOF and
/// skips the Start question too.
#[test]
fn ceiling_ridge_end_snap_reanchors_a_typed_length_at_the_wall() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    // Vertical stroke y 5.0 → 9.9: hi snaps to y = 10, lo (1 m from the
    // interior wall y = 4) does not.
    c.apply_roof_ridge_stroke_event(&[(2.78, 5.0), (2.8, 9.9)]);
    assert!(
        c.ceiling_ridge_length_pending(),
        "length opens (one end free)"
    );
    assert!(
        c.ceiling_ridge_length_anchored(),
        "snapped hi anchors the extent"
    );
    let prefill = c.pending_roof_ridge_length().unwrap();
    assert!(
        (prefill - 5.0).abs() < 1e-4,
        "prefill runs drawn-lo → snapped-hi (got {prefill})"
    );
    // A typed 3 m re-anchors at the wall: lo = 10 − 3 = 7.
    c.assign_roof_ridge_length(3.0).expect("length re-anchors");
    let ((_, ay), (_, by)) = c.pending_roof_ridge_line().unwrap();
    assert!(
        (ay - 7.0).abs() < 1e-6 && (by - 10.0).abs() < 1e-6,
        "far end pinned to the wall, start re-anchored"
    );
    c.skip_position_dimension().unwrap(); // cross: drawn stands
    assert!(
        c.ceiling_ridge_height_pending(),
        "start skipped — the hi snap fixed the position"
    );
    c.assign_roof_ridge_height(4.0).expect("gable end assigns");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    for (apex, want_y) in storey.ceiling_apexes.iter().zip([7.0, 10.0]) {
        assert!((apex.point.y - want_y).abs() < 1e-6);
    }
}

/// The SAME-WALL guard: a short stroke with both ends in tolerance of ONE
/// wall keeps only the NEARER end's snap — the other end stays drawn, so
/// the ridge never collapses to a point on the wall.
#[test]
fn ceiling_ridge_same_wall_snap_keeps_only_the_nearer_end() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    // Both ends within 0.2 of y = 4 (the interior wall): lo at 3.9 (0.1
    // away), hi at 4.15 (0.15 away) — the LO end is nearer and keeps it.
    c.apply_roof_ridge_stroke_event(&[(2.78, 3.9), (2.8, 4.15)]);
    assert!(c.ceiling_ridge_length_pending());
    let ((_, ay), (_, by)) = c.pending_roof_ridge_line().unwrap();
    assert!((ay - 4.0).abs() < 1e-6, "nearer LO end snapped to the wall");
    assert!(
        (by - 4.25).abs() < 1e-4,
        "HI end keeps the drawn length from the snapped lo (got {by})"
    );
    c.cancel_pending_roof_ridge();
}

/// THE UN-SNAP TRANSITION TABLE (design §2, normative), table-driven: which
/// tag shows where, what its "Don't snap" reverts, and which question
/// reopens. END → the Length question (always), surviving snaps ride;
/// START → the start question (resolved length + cross stand); CROSS → the
/// cross question, restarting through Start (resolved length stands).
#[test]
fn ceiling_ridge_unsnap_transition_table() {
    // The pending ridge's stage as a short label (vertical strokes only:
    // an X-axis prompt is the cross question, Y the start).
    fn stage(c: &AppController) -> &'static str {
        if c.ceiling_ridge_length_pending() {
            return "len";
        }
        if c.ceiling_ridge_height_pending() {
            return "height";
        }
        if c.ceiling_ridge_position_pending() {
            let ui = c.position_prompt_for_ui().expect("prompt at a position");
            return match ui.axis {
                ParameterAxis::X => "cross",
                ParameterAxis::Y => "start",
            };
        }
        "none"
    }
    // (name, stroke, skips to reach the tag's stage, the stage the tag
    //  shows at, tag position (x, y), stage after un-snap, the reverted
    //  drawn coordinate as (axis-of-line-check, expected value)).
    struct Row {
        name: &'static str,
        stroke: [(f32, f32); 2],
        skips: usize,
        tag_stage: &'static str,
        tag_pos: (f32, f32),
        after: &'static str,
        // Check on the pending line after un-snap: (what, expected).
        revert: fn(&AppController) -> f64,
        expect: f64,
    }
    let rows = [
        Row {
            // END tag at the HEIGHT stage (end-only snap): un-snap reverts
            // hi to the drawn 9.9 and reopens the LENGTH question.
            name: "end tag at height",
            stroke: [(2.78, 5.0), (2.8, 9.9)],
            skips: 2, // length, cross (start auto-skipped) → height
            tag_stage: "height",
            tag_pos: (2.8, 10.0),
            after: "len",
            revert: |c| c.pending_roof_ridge_line().unwrap().1 .1, // hi
            expect: 9.9,
        },
        Row {
            // END tag at the CROSS stage (both ends snapped — the Length
            // was skipped by construction): un-snap reopens the LENGTH;
            // the START snap rides (the extent stays anchored at y = 0).
            name: "end tag at cross",
            stroke: [(2.78, 0.1), (2.8, 9.9)],
            skips: 0, // straight to the cross question
            tag_stage: "cross",
            tag_pos: (2.8, 10.0),
            after: "len",
            revert: |c| c.pending_roof_ridge_line().unwrap().1 .1, // hi
            expect: 9.9,
        },
        Row {
            // START tag at the HEIGHT stage (start-only snap): un-snap
            // reverts lo to the drawn 4.1 and reopens the START question.
            name: "start tag at height",
            stroke: [(2.78, 4.1), (2.8, 8.5)],
            skips: 2, // length, cross → height (start was skipped)
            tag_stage: "height",
            tag_pos: (2.8, 4.0),
            after: "start",
            revert: |c| c.pending_roof_ridge_line().unwrap().0 .1, // lo
            expect: 4.1,
        },
        Row {
            // CROSS tag at the START stage: un-snap reverts the coordinate
            // to the drawn 5.9 and restarts from the CROSS question.
            name: "cross tag at start",
            stroke: [(5.88, 0.5), (5.9, 3.5)],
            skips: 1, // length → start (cross was skipped)
            tag_stage: "start",
            tag_pos: (6.0, 2.0),
            after: "cross",
            revert: |c| c.pending_roof_ridge_line().unwrap().0 .0, // coord
            expect: 5.9,
        },
        Row {
            // CROSS tag at the HEIGHT stage (nothing else snapped):
            // un-snap restarts from the CROSS question through Start.
            name: "cross tag at height",
            stroke: [(5.88, 0.5), (5.9, 3.5)],
            skips: 2, // length, start → height
            tag_stage: "height",
            tag_pos: (6.0, 2.0),
            after: "cross",
            revert: |c| c.pending_roof_ridge_line().unwrap().0 .0, // coord
            expect: 5.9,
        },
    ];
    for row in rows {
        let mut c = ceiling_ready_l_controller();
        c.set_roof_point_mode(true);
        c.apply_roof_ridge_stroke_event(&row.stroke);
        for _ in 0..row.skips {
            if c.ceiling_ridge_length_pending() {
                c.skip_roof_ridge_length()
                    .unwrap_or_else(|e| panic!("{}: length skip rejected: {e}", row.name));
            } else {
                c.skip_position_dimension().unwrap();
            }
        }
        assert_eq!(stage(&c), row.tag_stage, "{}: tag stage", row.name);
        let tag = c
            .ceiling_ridge_snap_tag()
            .unwrap_or_else(|| panic!("{}: tag missing", row.name));
        assert!(
            (tag.pos.0 - row.tag_pos.0).abs() < 0.05 && (tag.pos.1 - row.tag_pos.1).abs() < 0.05,
            "{}: tag at {:?}, want {:?}",
            row.name,
            tag.pos,
            row.tag_pos
        );
        c.unsnap_roof_ridge();
        assert_eq!(stage(&c), row.after, "{}: reopened question", row.name);
        let got = (row.revert)(&c);
        assert!(
            (got - row.expect).abs() < 0.05,
            "{}: reverted to {got}, want {}",
            row.name,
            row.expect
        );
        // One tag at a time: the clicked tag is gone. (Another snap's tag
        // may surface later, once its question is passed again — but never
        // at the reopened question itself for the un-snapped DOF.)
        if let Some(t) = c.ceiling_ridge_snap_tag() {
            assert!(
                (t.pos.0 - tag.pos.0).abs() > 0.05 || (t.pos.1 - tag.pos.1).abs() > 0.05,
                "{}: the un-snapped tag must not still show",
                row.name
            );
        }
    }
}

/// Finalize at the CROSS stage: when the start snapped, the cross question
/// is the LAST open location stage, so a length that fits the bbox but not
/// the run from the snapped start is rejected on the typed CROSS distance —
/// with the permitted run — and an achievable cross then lands.
#[test]
fn ceiling_ridge_finalize_rejection_lands_on_the_cross_when_start_snapped() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    // Start snaps to y = 4; the upper arm's run from there is 6 m (y 4..10).
    c.apply_roof_ridge_stroke_event(&[(2.78, 4.1), (2.8, 8.5)]);
    // 6.5 m fits the L's 10 m bbox → passes the Length stage's bound.
    c.assign_roof_ridge_length(6.5)
        .expect("optimistic bound passes");
    let ui = c.position_prompt_for_ui().expect("cross question");
    assert_eq!(ui.axis, ParameterAxis::X);
    // The cross answer completes the geometry → finalize: anchored at the
    // snapped y = 4, the 6.5 m extent reaches y = 10.5 — half a metre past
    // the far wall at x = 2.5 — rejected on the typed cross with the full
    // run there (the L spans y 0..10 at that x), prompt stays open.
    let err = c.apply_position_dimension(2.5).unwrap_err();
    assert!(err.contains("out of its ceiling area"), "got: {err}");
    assert!(err.contains("10.00 m wall to wall"), "got: {err}");
    assert!(
        c.ceiling_ridge_position_pending(),
        "cross prompt stays open"
    );
    c.cancel_pending_roof_ridge();
    assert!(!c.ceiling_ridge_pending());
}

/// A storey switch abandons the in-flight ridge and leaves the mode — its
/// coordinates are in the outgoing storey's space.
#[test]
fn storey_switch_clears_pending_roof_ridge() {
    let mut c = ceiling_apex_ready_controller();
    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    assert!(c.ceiling_ridge_pending());
    c.add_storey().unwrap();
    assert!(!c.ceiling_point_mode(), "mode reset on switch");
    assert!(!c.ceiling_ridge_pending(), "pending ridge abandoned");
}

// ---------------------------------------------------------------------------
// Home / workspace shell wiring: list, delete, rename, date, exit.
// ---------------------------------------------------------------------------

/// `list_bers` returns saved records newest-assessment-first, `delete_ber`
/// removes them, and the currently-open record is refused (Home is only
/// reachable record-free via `exit_ber`, but the guard keeps it explicit).
#[test]
fn list_and_delete_bers() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let a = c.create_ber(
        "A".to_string(),
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    assert!(
        c.delete_ber(&a.id).is_err(),
        "deleting the open record must be refused"
    );
    assert_eq!(c.list_bers().len(), 1, "refused delete must not remove it");
    c.exit_ber().unwrap();
    let b = c.create_ber(
        "B".to_string(),
        NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
    );
    c.exit_ber().unwrap();

    let list = c.list_bers();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].name, "B", "newest assessment date first");
    assert_eq!(list[1].name, "A");

    c.delete_ber(&a.id).unwrap();
    let list = c.list_bers();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, b.id);
}

/// Key Details live edits (rename + date + phases) write through to the
/// record and survive an exit → `open_ber` round-trip, i.e. they persisted.
#[test]
fn rename_and_date_persist_through_reload() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let ber = c.create_ber(
        "Untitled BER".to_string(),
        NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
    );
    c.rename_ber("12 Main Street".to_string());
    c.set_date_assessed(NaiveDate::from_ymd_opt(2026, 6, 5).unwrap());
    let mut phases = ConstructionPhases::default();
    phases.set_year(ConstructionPhase::Original, 1985);
    c.set_construction_phases(phases);
    c.exit_ber().unwrap();
    assert!(c.current_key_details().is_none(), "exit clears the record");

    c.open_ber(&ber.id).unwrap();
    let (name, date, phases) = c.current_key_details().unwrap();
    assert_eq!(name, "12 Main Street");
    assert_eq!(date, NaiveDate::from_ymd_opt(2026, 6, 5).unwrap());
    assert_eq!(phases.year_of(&ConstructionPhase::Original), Some(1985));
}

/// `exit_ber` clears ALL loaded/session state so a subsequent create starts
/// clean: no record, no plan, default modes; the new record then starts on
/// storey 0 (the `Some(0)` reset belongs to create/open, not exit).
#[test]
fn exit_then_create_starts_clean() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    c.create_ber(
        "A".to_string(),
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    // Leave editing state behind: a drawn plan + a toggled mode.
    let square = vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(10.0, 0.0),
        Point2f32::new(10.0, 10.0),
        Point2f32::new(0.0, 10.0),
        Point2f32::new(0.2, 0.2),
    ];
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
    c.set_living_area_mode(true);

    c.exit_ber().unwrap();
    assert!(c.current_record_name().is_none());
    assert_eq!(c.current_storey_idx(), None);
    assert!(!c.has_floor_plan(), "exit must drop the drawn plan");
    assert!(!c.living_area_mode(), "exit must reset interaction modes");

    let b = c.create_ber(
        "B".to_string(),
        NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
    );
    assert_eq!(c.current_record_name().as_deref(), Some("B"));
    assert_eq!(c.current_storey_idx(), Some(0));
    assert!(
        !c.has_floor_plan(),
        "fresh record must start without a plan"
    );
    assert_eq!(c.current_key_details().unwrap().0, "B");
    let _ = b;
}

/// Drag-to-orbit view state: the defaults are the documented isometric
/// baseline (yaw 45°, pitch atan(1/√2) ≈ 35.264° — the angles that
/// reproduce the historical fixed projection), yaw wraps mod 360, pitch
/// clamps to 5..=90 (90 = top-down plan), and Fit/reset restores the
/// defaults along with zoom/pan.
#[test]
fn orbit_view_wraps_yaw_clamps_pitch_and_resets() {
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let v = c.view();
    assert_eq!(
        v.yaw_deg, DEFAULT_YAW_DEG,
        "default yaw is the 45° baseline"
    );
    assert_eq!(v.pitch_deg, DEFAULT_PITCH_DEG);
    let true_iso_pitch = (1.0f64 / 2.0f64.sqrt()).atan().to_degrees();
    assert!(
        (DEFAULT_PITCH_DEG as f64 - true_iso_pitch).abs() < 1e-3,
        "default pitch is the true-isometric elevation atan(1/√2): \
         {DEFAULT_PITCH_DEG} vs {true_iso_pitch}"
    );

    // Yaw wraps mod 360 in both directions.
    c.orbit_view(350.0, 0.0); // 45 + 350 = 395 → 35
    assert!((c.view().yaw_deg - 35.0).abs() < 1e-3, "yaw wraps past 360");
    c.orbit_view(-90.0, 0.0); // 35 − 90 = −55 → 305
    assert!((c.view().yaw_deg - 305.0).abs() < 1e-3, "yaw wraps below 0");

    // Pitch clamps: top-down plan at 90, just-above-horizon at 5.
    c.orbit_view(0.0, 500.0);
    assert_eq!(c.view().pitch_deg, 90.0, "pitch clamps at top-down");
    c.orbit_view(0.0, -500.0);
    assert_eq!(c.view().pitch_deg, 5.0, "pitch clamps above the horizon");

    // Fit (reset_view) restores the default angles along with zoom/pan.
    c.pan_view(10.0, -4.0);
    c.zoom_view(2.0, (0.0, 0.0), (0.0, 0.0));
    c.reset_view();
    let v = c.view();
    assert_eq!(v.yaw_deg, DEFAULT_YAW_DEG, "reset restores the default yaw");
    assert_eq!(v.pitch_deg, DEFAULT_PITCH_DEG);
    assert_eq!((v.zoom, v.offset_x, v.offset_y), (1.0, 0.0, 0.0));
}

// ─── Live-session repro: L-shaped storey, snapped ceiling step, wall tops ────

/// Replay of a reported live session up to the ceiling step: the L-shaped
/// outline drawn in pixels (6 orthogonal vertices), dimensioned 3 / 2 / 1 / 1
/// through the real prompt loop, every perimeter wall marked Exposed, default
/// ceiling height 2.4 m. Returns the controller plus the storey's landmark
/// coordinates (min/max x/y, the inner vertical wall's x, the inner
/// horizontal wall's y) measured from the dimensioned outline.
fn users_l_session_controller() -> (AppController, [f32; 6]) {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    let _ = c.create_ber(
        "T".into(),
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    );
    // The user's orthogonalized outline (pixel space), closed ring.
    let ring_px = [
        p(297.54297, 783.47656),
        p(1029.625, 783.47656),
        p(1029.625, 365.73828),
        p(682.5703, 365.73828),
        p(682.5703, 147.86328),
        p(297.54297, 147.86328),
    ];
    let mut stroke = ring_px.to_vec();
    stroke.push(p(298.0, 782.5)); // lift near start
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(&stroke, 0.5, true).unwrap();
    assert_eq!(
        c.document.floor_plan.polygons()[0].len(),
        6,
        "outline keeps its 6 corners"
    );
    // Map each wall (by stable start VertexId) to the user's typed length:
    // ring edges 0..6 got 3 / 2 / 1 / 1 (+ the two closure-derived 2 / 3).
    let targets_by_edge = [3.0_f32, 2.0, 1.0, 1.0, 2.0, 3.0];
    let mut target_of: std::collections::HashMap<VertexId, f32> = std::collections::HashMap::new();
    for (vid, a, b) in c.document.floor_plan.perimeter_walls() {
        let mid = ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        let edge = (0..6)
            .min_by(|&i, &j| {
                let m = |k: usize| {
                    let (s, e) = (ring_px[k], ring_px[(k + 1) % 6]);
                    let (mx, my) = ((s.x + e.x) / 2.0, (s.y + e.y) / 2.0);
                    (mx - mid.0).powi(2) + (my - mid.1).powi(2)
                };
                m(i).partial_cmp(&m(j)).unwrap()
            })
            .unwrap();
        target_of.insert(vid, targets_by_edge[edge]);
    }
    // The real dimensioning loop: answer each prompt with that wall's length.
    c.compute_next_dimension_prompt();
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        let prompt = c.document.dimensioning.current_prompt.clone().unwrap();
        let v = target_of[&prompt.wall_start_vertex];
        c.apply_dimension(v)
            .unwrap_or_else(|e| panic!("dimension {v} failed: {e}"));
        guard += 1;
    }
    assert!(guard < 12, "dimension loop didn't terminate");
    c.commit_to_ber().unwrap();
    assert!(c.outline_finalised(), "outline fully dimensioned");
    c.set_default_ceiling_height(2.4).expect("height applies");
    c.set_all_remaining_exposed();
    c.commit_to_ber().unwrap();

    // Landmarks from the dimensioned outline (world metres).
    let pts = c.document.outline.floor_plan.get_points();
    let xs: Vec<f32> = pts.iter().map(|p| p.x).collect();
    let ys: Vec<f32> = pts.iter().map(|p| p.y).collect();
    let min_x = xs.iter().copied().fold(f32::INFINITY, f32::min);
    let max_x = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let min_y = ys.iter().copied().fold(f32::INFINITY, f32::min);
    let max_y = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let inner_x = xs
        .iter()
        .copied()
        .find(|&x| (x - min_x).abs() > 1e-3 && (x - max_x).abs() > 1e-3)
        .expect("inner vertical wall");
    let inner_y = ys
        .iter()
        .copied()
        .find(|&y| (y - min_y).abs() > 1e-3 && (y - max_y).abs() > 1e-3)
        .expect("inner horizontal wall");
    (c, [min_x, max_x, min_y, max_y, inner_x, inner_y])
}

/// The user's ceiling step: a near-vertical stroke drifting from
/// (3.217, 1.215) to (3.317, 3.261) — fits at x ≈ 3.27, within the 0.2 m snap
/// tolerance of the inner wall, so it SNAPS onto it — then the jut side
/// (x > inner wall) tapped and raised by +1.0 m.
fn apply_users_ceiling_step(
    c: &mut AppController,
    inner_x: f32,
    inner_y: f32,
    max_x: f32,
    max_y: f32,
) {
    c.set_ceiling_step_mode(true);
    let stroke: Vec<(f32, f32)> = (0..=10)
        .map(|i| {
            let t = i as f32 / 10.0;
            (3.217 + 0.100 * t, 1.215 + 2.046 * t)
        })
        .collect();
    c.apply_ceiling_step_stroke_event(&stroke);
    assert!(c.ceiling_step_side_pending(), "stroke fitted → side pick");
    let ((lx, _), _) = c.pending_ceiling_step_line().unwrap();
    assert!(
        (lx as f32 - inner_x).abs() < 1e-4,
        "line snapped onto the inner wall: line at {lx}, wall at {inner_x}"
    );
    c.ceiling_step_tap((inner_x + max_x) / 2.0, (inner_y + max_y) / 2.0);
    assert!(
        c.ceiling_step_amount_pending(),
        "snapped line skips the position prompt"
    );
    c.assign_ceiling_step_delta(1.0).expect("step applies");
}

/// THE MODEL QUESTION from the live report: after the ceiling-step line
/// SNAPPED onto the outline's inner vertical wall and the jut side was raised
/// by +1.0 m, do the resolved wall regions around the raised section top out
/// at 3.4 m — or stay flat at the 2.4 m default? Extended for slice C: a
/// floor step lowering the same jut by 0.3 m derives a riser along the step
/// line and the ceiling step derives another there — both defaulting `Unset`
/// like every other wall — with correct z-spans, unique ids, and identity
/// stable across reload.
#[test]
fn users_l_session_raised_section_wall_tops_follow_the_step() {
    let (mut c, [min_x, max_x, min_y, max_y, inner_x, inner_y]) = users_l_session_controller();
    println!(
        "landmarks: x [{min_x:.4}, {inner_x:.4}, {max_x:.4}]  y [{min_y:.4}, {inner_y:.4}, {max_y:.4}]"
    );

    apply_users_ceiling_step(&mut c, inner_x, inner_y, max_x, max_y);
    // The user also drops the jut's FLOOR by 0.3 m (same footprint as the
    // raised ceiling side: x > inner wall) — the floor-step counterpart.
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    c.step_regions.entry(0).or_default().push(StepRegion {
        polygon: vec![
            p(inner_x, inner_y),
            p(max_x, inner_y),
            p(max_x, max_y),
            p(inner_x, max_y),
        ],
        level_m: -0.3,
    });
    // The direct push above doesn't bump the revision — force the derive.
    c.commit_to_ber().unwrap();

    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    println!("{}", document::debug_dump_storey(storey, "users-L"));

    // Ceilings: raised piece at 3.4 over the jut, remainder at 2.4.
    let zs: Vec<f64> = storey
        .ceilings
        .regions
        .iter()
        .filter_map(|r| r.flat_z())
        .collect();
    assert!(
        zs.iter().any(|z| (z - 3.4).abs() < 1e-6) && zs.iter().any(|z| (z - 2.4).abs() < 1e-6),
        "roof split into 2.4 + 3.4 pieces: {zs:?}"
    );

    // Walls: classify each marked wall by its base edge and check its top.
    let top_zs = |r: &ber_model::ber::structural::WallRegion| -> Vec<f64> {
        r.polygon
            .exterior
            .iter()
            .skip(2) // base edge first (the contract), then the top profile
            .map(|v| v.z)
            .collect()
    };
    // 6 perimeter walls + the 2 derived RISER faces along the step line.
    assert_eq!(storey.walls.regions.len(), 8, "six walls + two risers");
    // A riser's base edge lies ON the step line x = inner_x, over the
    // INTERIOR run y ∈ [inner_y, max_y] (the outline's own inner wall sits
    // below inner_y).
    let near = |v: f64, w: f32| (v - w as f64).abs() < 1e-3;
    let is_riser = |r: &ber_model::ber::structural::WallRegion| {
        let e = &r.polygon.exterior;
        near(e[0].x, inner_x) && near(e[1].x, inner_x) && e[0].y.max(e[1].y) > inner_y as f64 + 1e-3
    };
    for r in &storey.walls.regions {
        if is_riser(r) {
            continue; // asserted separately below
        }
        let e = &r.polygon.exterior;
        let (a, b) = (e[0], e[1]);
        let tops = top_zs(r);
        let on_raised_side = |x: f64| x > inner_x as f64 + 1e-3;
        if near(a.x, max_x) && near(b.x, max_x) {
            // Right wall of the jut: fully raised.
            assert!(
                tops.iter().all(|z| (z - 3.4).abs() < 1e-6),
                "right jut wall must top at 3.4: {tops:?}"
            );
        } else if near(a.y, inner_y) && near(b.y, inner_y) && on_raised_side((a.x + b.x) / 2.0) {
            // Bottom wall of the jut: fully raised.
            assert!(
                tops.iter().all(|z| (z - 3.4).abs() < 1e-6),
                "jut bottom wall must top at 3.4: {tops:?}"
            );
        } else if near(a.y, max_y) && near(b.y, max_y) {
            // Long top wall: crosses the step → both heights, jump at inner_x.
            assert!(
                tops.iter().any(|z| (z - 3.4).abs() < 1e-6)
                    && tops.iter().any(|z| (z - 2.4).abs() < 1e-6),
                "top wall must jump 2.4 → 3.4 at the step: {tops:?}"
            );
        } else {
            // Everything else stays at the default height.
            assert!(
                tops.iter().all(|z| (z - 2.4).abs() < 1e-6),
                "wall ({:.3},{:.3})→({:.3},{:.3}) outside the raised side must stay at 2.4: {tops:?}",
                a.x, a.y, b.x, b.y
            );
        }
    }

    // THE RISERS along the step line (slice C): the floor step's riser
    // spanning −0.3 → 0 and the ceiling step's riser spanning 2.4 → 3.4,
    // each one face over y ∈ [inner_y, max_y], BOTH defaulting Unset (the
    // riser default is Unset like every other wall — so tell them apart by
    // z-span, not exposure).
    let risers: Vec<&ber_model::ber::structural::WallRegion> = storey
        .walls
        .regions
        .iter()
        .filter(|r| is_riser(r))
        .collect();
    assert_eq!(risers.len(), 2, "one floor + one ceiling riser");
    assert!(
        risers.iter().all(|r| r.exposure == WallExposure::Unset),
        "fresh risers default Unset like every other wall"
    );
    let span = |r: &ber_model::ber::structural::WallRegion| {
        let zs = r.polygon.exterior.iter().map(|v| v.z);
        (
            zs.clone().fold(f64::INFINITY, f64::min),
            zs.fold(f64::NEG_INFINITY, f64::max),
        )
    };
    let floor_riser = risers
        .iter()
        .find(|r| span(r).1 < 1.0)
        .expect("the floor step derives a riser");
    let (lo, hi) = span(floor_riser);
    assert!(
        (lo + 0.3).abs() < 1e-6 && hi.abs() < 1e-6,
        "floor riser spans the floor step −0.3 → 0: [{lo}, {hi}]"
    );
    let ceiling_riser = risers
        .iter()
        .find(|r| span(r).0 > 1.0)
        .expect("the ceiling step derives a riser");
    let (lo, hi) = span(ceiling_riser);
    assert!(
        (lo - 2.4).abs() < 1e-3 && (hi - 3.4).abs() < 1e-3,
        "ceiling riser spans the ceiling step 2.4 → 3.4: [{lo}, {hi}]"
    );
    for r in &risers {
        let ys = [r.polygon.exterior[0].y, r.polygon.exterior[1].y];
        assert!(
            near(ys[0].min(ys[1]), inner_y) && near(ys[0].max(ys[1]), max_y),
            "riser runs the full interior step line: {ys:?}"
        );
        // Base-edge contract: exterior[0]/[1] at the riser's LOWER level.
        let (lo, _) = span(r);
        assert!((r.polygon.exterior[0].z - lo).abs() < 1e-6);
        assert!((r.polygon.exterior[1].z - lo).abs() < 1e-6);
    }
    // No id is duplicated across the perimeter + riser regions.
    let mut ids: Vec<_> = storey.walls.regions.iter().map(|r| r.id).collect();
    ids.sort_by_key(|id| id.0);
    ids.dedup();
    assert_eq!(ids.len(), 8, "ids unique");

    // And the raised tops + risers SURVIVE persist → reload → re-derive (the
    // session step areas are reconstructed from the persisted floors/ceilings;
    // without that the next commit would flatten the walls back to 2.4).
    c.commit_to_ber().unwrap();
    let id = c.current_record.as_ref().unwrap().id;
    c.open_ber(&id).unwrap();
    c.commit_to_ber().unwrap();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let raised_tops = storey
        .walls
        .regions
        .iter()
        .flat_map(|r| r.polygon.exterior.iter().skip(2))
        .filter(|v| (v.z - 3.4).abs() < 1e-3)
        .count();
    assert!(
        raised_tops >= 4,
        "raised wall tops survive reload + re-derive: {}",
        document::debug_dump_storey(storey, "users-L-reloaded")
    );
    // Riser identity is stable across the round trip (amend matching), and
    // the reload left the painted wall sections untouched — the risers never
    // turn into phantom exposure sections.
    let reloaded: Vec<&ber_model::ber::structural::WallRegion> = storey
        .walls
        .regions
        .iter()
        .filter(|r| is_riser(r))
        .collect();
    assert_eq!(reloaded.len(), 2, "both risers re-derive after reload");
    for sections in c.document.wall_sections.values() {
        assert!(
            sections.iter().all(|s| s.exposure == WallExposure::Exposed),
            "only the painted Exposed sections recover — no riser phantoms"
        );
    }
    assert_eq!(
        c.document.wall_sections.len(),
        6,
        "exactly the six painted perimeter walls have sections"
    );
}

/// Structure phase: assigning a wall construction to a RISER (interior step face)
/// must stick — risers have no perimeter SurfaceRef, so they're keyed by their
/// base edge like riser exposure. Regression: previously the assign + stamp
/// silently skipped risers, so "Walls" never went green when a step existed.
#[test]
fn riser_walls_receive_and_keep_a_structure_construction() {
    use ber_model::ber::structural::{ConstructionMakeup, SurfaceKind, WallConstructionType};
    let (mut c, [_min_x, max_x, _min_y, max_y, inner_x, inner_y]) = users_l_session_controller();
    apply_users_ceiling_step(&mut c, inner_x, inner_y, max_x, max_y);
    // Drop the jut's floor too → a floor-step riser as well as the ceiling one.
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    c.step_regions.entry(0).or_default().push(StepRegion {
        polygon: vec![
            p(inner_x, inner_y),
            p(max_x, inner_y),
            p(max_x, max_y),
            p(inner_x, max_y),
        ],
        level_m: -0.3,
    });
    c.commit_to_ber().unwrap();

    // Assign Timber + Default to EVERY wall, including the two interior risers.
    let ids: Vec<_> = c.current_record.as_ref().unwrap().data.storeys[0]
        .walls
        .regions
        .iter()
        .map(|r| r.id)
        .collect();
    c.set_wall_construction_for(
        &ids,
        WallConstructionType::TimberConstruction,
        ConstructionMakeup::Default,
        ConstructionPhase::default(),
    );
    c.commit_to_ber().unwrap();

    // Now every wall — risers included — carries a complete construction, so the
    // Structure "Walls" row is done. Previously the risers stayed incomplete.
    let ber = &c.current_record.as_ref().unwrap().data;
    assert!(
        ber.structure_surfaces_complete(0, SurfaceKind::Walls),
        "all walls incl. risers complete: {}",
        document::debug_dump_storey(&ber.storeys[0], "riser-construction")
    );

    // And it survives persist → reload → re-derive (riser key seeded from record).
    let id = c.current_record.as_ref().unwrap().id;
    c.open_ber(&id).unwrap();
    c.commit_to_ber().unwrap();
    assert!(
        c.current_record
            .as_ref()
            .unwrap()
            .data
            .structure_surfaces_complete(0, SurfaceKind::Walls),
        "riser construction survives reload"
    );
}

/// The END-OF-SESSION ridge from the live report, under the Length-first
/// order: a near-vertical ridge stroke at x ≈ 3.73–3.78 (y 1.66 → 2.67)
/// over the RAISED jut (its ceiling piece runs 2.0 m, inner_y → max_y, at
/// 3.4). The LENGTH question opens first and its bound is OPTIMISTIC (the
/// typed 1.5 m fits several areas' bboxes, so it passes); the cross is then
/// skipped (the drawn x stands), and the START — the last open location
/// stage — runs the binding placement check: from lo = inner_y + 1.0 the
/// raised area's far wall is exactly 1.0 m away, so the 1.5 m ridge is
/// rejected ON THE TYPED START with the permitted run; a start the length
/// fits from is accepted and the Height stage then SUCCEEDS — the
/// no-late-rejection invariant in its honest form, end to end.
#[test]
fn users_l_session_ridge_placement_is_limited_to_the_raised_area_and_height_lands() {
    let (mut c, [_, max_x, _, max_y, inner_x, inner_y]) = users_l_session_controller();
    apply_users_ceiling_step(&mut c, inner_x, inner_y, max_x, max_y);

    c.set_ceiling_step_mode(false);
    c.set_roof_point_mode(true);
    let stroke: Vec<(f32, f32)> = (0..=10)
        .map(|i| {
            let t = i as f32 / 10.0;
            (3.73 + 0.05 * t, 1.66 + 1.01 * t)
        })
        .collect();
    c.apply_roof_ridge_stroke_event(&stroke);
    // Question 1: the length. A wildly long value is rejected HERE (it
    // fits no area's bbox — the optimistic bound)…
    assert!(c.ceiling_ridge_length_pending(), "length question opens");
    let err = c.assign_roof_ridge_length(25.0).unwrap_err();
    assert!(
        err.contains("No ceiling area is that long"),
        "beyond every bbox → the Length-stage bound: {err}"
    );
    assert!(
        c.ceiling_ridge_length_pending(),
        "pending intact on rejection"
    );
    // …but 1.5 m fits SOME area's bbox, so it passes — the binding check
    // is the location stage's.
    c.assign_roof_ridge_length(1.5)
        .expect("a bbox-fitting length passes the Length stage");

    // Neither drawn position is within snap tolerance (nearest parallel
    // walls sit ≈ 0.46 m / 0.45 m away) → the cross question opens.
    assert!(c.ceiling_ridge_position_pending(), "cross question opens");
    c.skip_position_dimension().unwrap(); // cross: the drawn x ≈ 3.75 stands
    assert!(c.ceiling_ridge_position_pending(), "start question opens");

    // The START is the last open location stage: from lo = inner_y + 1.0
    // the 1.5 m ridge overruns the raised area's far wall by half a metre
    // — rejected on the typed start, WITH the area's full 2.0 m run, and
    // the prompt stays open.
    let err = c.apply_position_dimension(1.0).unwrap_err();
    assert!(
        err.contains("out of its ceiling area"),
        "stable prefix kept: {err}"
    );
    assert!(
        err.contains("2.00 m wall to wall"),
        "permitted run in the message: {err}"
    );
    assert!(
        c.ceiling_ridge_position_pending(),
        "start prompt stays open on the finalize rejection"
    );

    // A start the length fits from (0.3 m above inner_y leaves a 1.7 m
    // run): accepted…
    c.apply_position_dimension(0.3)
        .expect("a start the length fits from lands");
    assert!(
        c.ceiling_ridge_height_pending(),
        "on to the height question"
    );
    // …and the Height stage must then SUCCEED: finalize acceptance implies
    // the height validation's strict same-region containment.
    c.assign_roof_ridge_height(4.2)
        .expect("no late rejection at the height stage");
    assert!(!c.ceiling_ridge_pending(), "flow complete");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.ceiling_apexes.len(), 2, "both apexes durable");
}

// ─── Riser exposure: the 2D dashed-line picker ──────────────────────────────

/// Helper: the 10×10 dimensioned storey with a floor step over its left half
/// — derives one riser wall along x = 5 (defaulting `Unset`) — materialized
/// and ready for exposure-mode riser picking.
fn riser_ready_controller() -> AppController {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    c.step_regions.entry(0).or_default().push(StepRegion {
        polygon: vec![p(0., 0.), p(5., 0.), p(5., 10.), p(0., 10.)],
        level_m: -0.3,
    });
    c.commit_to_ber().unwrap();
    c
}

/// "Set remaining → Exposed" covers RISER faces too: an Unset riser becomes
/// `Exposed` alongside the outline walls' unset coverage, while a riser the
/// assessor already classified is untouched (same "painted stays" rule as
/// the outline sections).
#[test]
fn set_all_remaining_exposed_includes_unset_risers() {
    let mut c = riser_ready_controller();
    c.set_exposure_mode(true);
    let views = c.riser_views();
    assert_eq!(views.len(), 1, "one riser on the step line");
    assert_eq!(views[0].exposure, WallExposure::Unset);

    c.set_all_remaining_exposed();
    assert_eq!(
        c.riser_views()[0].exposure,
        WallExposure::Exposed,
        "the bulk pass classified the riser"
    );
    // Every outline wall's coverage is painted too (the pre-existing part).
    for w in c.wall_exposure_views() {
        assert!(
            document::wall_coverage(w.length_m, &w.sections)
                .iter()
                .all(|(_, _, e)| *e != WallExposure::Unset),
            "no unset outline coverage remains"
        );
    }

    // An already-classified riser is untouched by a later bulk pass.
    let id = c.riser_views()[0].id;
    c.selected_riser = Some(id);
    c.set_riser_exposure(WallExposure::NonHeatLoss);
    c.selected_riser = None;
    c.set_all_remaining_exposed();
    assert_eq!(
        c.riser_views()[0].exposure,
        WallExposure::NonHeatLoss,
        "a classified riser is not repainted by the bulk pass"
    );
}

/// Tapping near a riser's dashed line (within the tighter riser tolerance)
/// selects IT — not an outline wall — and the exposure type buttons then
/// route to the riser: the override lands in the record, survives the next
/// re-resolve (the amend preserves it), AND survives a full DB reload. The
/// override is record-level state, so it sits OUTSIDE undo (like phases /
/// steps / roof apexes) — no history entry is pushed.
#[test]
fn riser_tap_select_override_and_reload_round_trip() {
    let mut c = riser_ready_controller();
    c.set_exposure_mode(true);

    let views = c.riser_views();
    assert_eq!(views.len(), 1, "one riser on the step line");
    assert_eq!(views[0].kind, RiserKind::Floor);
    assert_eq!(
        views[0].exposure,
        WallExposure::Unset,
        "risers default Unset like every other wall"
    );

    let riser_id = views[0].id;

    // Tap ON the step line, mid-storey: the riser wins the pick.
    c.exposure_pointer_down(5.0, 5.0, 0.5);
    assert_eq!(c.selected_riser, Some(riser_id), "riser selected");
    assert!(
        c.selected_exposure_wall.is_none() && c.exposure_sel.is_none(),
        "riser selection is exclusive with the wall selection"
    );
    assert!(
        c.riser_views()[0].selected,
        "selection surfaces in the view"
    );

    // The type buttons route to the riser. Record-level → outside undo.
    c.set_wall_exposure(WallExposure::SemiExposed);
    assert!(
        !c.can_undo(),
        "riser override pushes NO history entry (it lives in the record, \
         which a Document snapshot can't revert)"
    );
    c.materialize(); // re-derives: the amend must PRESERVE the override
    let views = c.riser_views();
    assert_eq!(views[0].exposure, WallExposure::SemiExposed);

    // Another full re-derive (commit) keeps it too.
    c.commit_to_ber().unwrap();
    assert_eq!(c.riser_views()[0].exposure, WallExposure::SemiExposed);

    // Reload: the persisted region carries the override; the first
    // re-derive after load amends against the persisted set and keeps it.
    let id = c.current_record.as_ref().unwrap().id;
    c.open_ber(&id).unwrap();
    assert_eq!(
        c.riser_views()[0].exposure,
        WallExposure::SemiExposed,
        "override survives the DB round-trip"
    );
    c.commit_to_ber().unwrap();
    let views = c.riser_views();
    assert_eq!(views.len(), 1);
    assert_eq!(
        views[0].exposure,
        WallExposure::SemiExposed,
        "…and the first re-derive after reload"
    );
}

/// "Clear" on a selected riser reverts it to the DERIVED default — `Unset`,
/// like every other wall — and the revert also sticks across the next
/// re-derive.
#[test]
fn riser_clear_reverts_to_the_derived_default() {
    let mut c = riser_ready_controller();
    c.set_exposure_mode(true);
    c.exposure_pointer_down(5.0, 5.0, 0.5);
    c.set_wall_exposure(WallExposure::Exposed);
    c.materialize();
    assert_eq!(c.riser_views()[0].exposure, WallExposure::Exposed);

    c.clear_wall_exposure(); // routes to clear_riser_exposure
    c.materialize();
    assert_eq!(
        c.riser_views()[0].exposure,
        WallExposure::Unset,
        "floor riser cleared back to Unset"
    );
    c.commit_to_ber().unwrap();
    assert_eq!(c.riser_views()[0].exposure, WallExposure::Unset);
}

/// GROUND is the riser-only explicit choice (the toolbar's Ground button —
/// DEAP ground contact applies to below-grade faces, which today only risers
/// can be): setting it routes like any other exposure, survives the
/// re-derive AND a full DB reload, and never recovers into painted wall
/// sections (the Ground skip in `sections_from_wall_regions`, on top of the
/// geometric riser mismatch).
#[test]
fn riser_ground_choice_persists_and_never_recovers_to_sections() {
    let mut c = riser_ready_controller();
    c.set_exposure_mode(true);
    c.exposure_pointer_down(5.0, 5.0, 0.5);
    c.set_wall_exposure(WallExposure::Ground);
    c.materialize();
    assert_eq!(
        c.riser_views()[0].exposure,
        WallExposure::Ground,
        "the explicit Ground choice survives the amend"
    );

    c.commit_to_ber().unwrap();
    let id = c.current_record.as_ref().unwrap().id;
    c.open_ber(&id).unwrap();
    assert_eq!(
        c.riser_views()[0].exposure,
        WallExposure::Ground,
        "Ground survives the DB round-trip"
    );
    assert!(
        c.document.wall_sections.is_empty(),
        "the Ground riser never recovers into painted wall sections"
    );
}

/// Pick priority: a tap on an outline wall still selects the WALL (the
/// riser only wins inside its tighter band around its own line), and a tap
/// on empty space clears the riser selection like the wall one.
#[test]
fn riser_pick_priority_and_deselection() {
    let mut c = riser_ready_controller();
    c.set_exposure_mode(true);

    // On the left outline wall (x = 0) — 5 m from the riser line.
    c.exposure_pointer_down(0.05, 5.0, 0.5);
    assert!(c.selected_exposure_wall.is_some(), "wall selected");
    assert!(c.selected_riser.is_none(), "riser not stolen");

    // On the riser line: selection moves to the riser, wall cleared.
    c.exposure_pointer_down(5.0, 5.0, 0.5);
    assert!(c.selected_riser.is_some());
    assert!(c.selected_exposure_wall.is_none());

    // Empty space clears everything.
    c.exposure_pointer_down(2.5, 2.5, 0.1);
    assert!(c.selected_riser.is_none(), "tap-away deselects the riser");
    assert!(c.selected_exposure_wall.is_none());

    // Leaving the mode clears it too.
    c.exposure_pointer_down(5.0, 5.0, 0.5);
    assert!(c.selected_riser.is_some());
    c.set_exposure_mode(false);
    assert!(c.selected_riser.is_none(), "mode exit clears the selection");
}

/// THE L-SCENARIO DISCRIMINATOR CHECK: on the L-shaped storey with a snapped
/// ceiling step at x = 6 — whose riser line is COLLINEAR with the x = 6
/// perimeter wall but doesn't lie on its segment — no perimeter wall is
/// ever misclassified as a riser, and (the coplanar-flush merge, live
/// report 2026-06) the riser doesn't survive as a separate region at all:
/// the RAISED side covers the x = 6 wall, so the riser face is a flush
/// continuation of that wall's plane sharing its bottom edge segment — it
/// merges INTO the wall, which resolves as ONE L-profiled region covering
/// the whole x = 6 plane. The picker accordingly lists nothing; the merged
/// face is painted through the wall's normal exposure flow.
#[test]
fn riser_views_do_not_misclassify_perimeter_walls_on_the_l_scenario() {
    let mut c = ceiling_ready_l_controller();
    c.materialize();
    assert!(
        c.riser_views().is_empty(),
        "no steps yet → no risers (perimeter regions never classify)"
    );

    // The snapped ceiling step from the L-scenario: line at x = 6, Less
    // side, +0.3 — its riser face runs along x = 6 for y ∈ [0, 4], on the
    // same LINE as the perimeter wall at x = 6 (y ∈ [4, 10]).
    c.apply_ceiling_step_stroke_event(&[(5.88, 0.5), (5.92, 4.0), (5.9, 9.5)]);
    c.ceiling_step_tap(2.0, 5.0); // Less side
    c.assign_ceiling_step_delta(0.3).expect("amount applies");
    c.materialize();

    assert!(
        c.riser_views().is_empty(),
        "the flush riser merged into the x = 6 wall — nothing to pick"
    );
    // The perimeter set is untouched: all 6 outline walls still present.
    assert_eq!(c.wall_exposure_views().len(), 6);
    // The x = 6 wall region is the L-profiled union: its base edge is the
    // unchanged perimeter segment y ∈ [4, 10], its polygon now reaches the
    // riser's extent y = 0 with the step band z ∈ [2.4, 2.7].
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let wall = storey
        .walls
        .regions
        .iter()
        .find(|r| {
            let e = &r.polygon.exterior;
            e.iter().all(|v| (v.x - 6.0).abs() < 1e-4)
        })
        .expect("the x = 6 wall resolves");
    let e = &wall.polygon.exterior;
    assert!(
        (e[0].y - 4.0).abs() < 1e-4 && (e[1].y - 10.0).abs() < 1e-4 && e[0].z.abs() < 1e-9,
        "base edge unchanged (the host wall's): {e:?}"
    );
    assert!(
        e.iter()
            .any(|v| v.y.abs() < 1e-4 && (v.z - 2.4).abs() < 1e-6),
        "the merged riser's far end is part of the wall ring: {e:?}"
    );
    assert_eq!(
        storey
            .walls
            .regions
            .iter()
            .filter(|r| {
                let e = &r.polygon.exterior;
                e.iter().all(|v| (v.x - 6.0).abs() < 1e-4)
            })
            .count(),
        1,
        "ONE region covers the whole x = 6 plane"
    );
}

// ── New-storey flow (thickness → method → suggest/draw → inherited seed) ───

/// Storey 0 = 10×10 (metres) with ceiling 2.4 and the right half (x ≥ 5)
/// stepped up to 2.7 — the "below" every new-storey test inherits from.
fn stepped_ceiling_below_controller() -> AppController {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).expect("height applies");
    let region = c
        .document
        .ceiling_step_region(ParameterAxis::X, 5.0, document::LineSide::Greater, 0.3)
        .expect("step region builds");
    c.ceiling_step_regions.entry(0).or_default().push(region);
    c.mark_dirty().unwrap();
    c.recompose_ceilings(0); // regions-as-truth: compose the step into the ceilings
    c.materialize();
    c.reconstruct_ceiling_step_regions_for(0); // canonicalize, assign parity
    c
}

/// A record SAVED WITH the durable field restores its step list on load WITHOUT
/// touching the geometry — even if the ceilings were (hypothetically) flattened,
/// the durable list is the source the editor restores.
#[test]
fn durable_steps_restore_without_inference() {
    let mut c = stepped_ceiling_below_controller();
    // Durable field is populated (one step). Restore must use it directly.
    c.ceiling_step_regions.clear();
    assert!(
        c.restore_ceiling_step_regions_for(0),
        "restore succeeds from the durable field"
    );
    assert_eq!(c.ceiling_step_regions.get(&0).map(|v| v.len()), Some(1));
}

/// `(level_m, plan area)` per floor region of storey `idx`, sorted — the
/// fingerprint the seed/fixpoint assertions compare.
fn floor_level_areas(c: &AppController, idx: usize) -> Vec<(f64, f64)> {
    let storey = &c.current_record.as_ref().unwrap().data.storeys[idx];
    let mut v: Vec<(f64, f64)> = storey
        .floors
        .regions
        .iter()
        .zip(storey.floors.to_vertex_lists())
        .map(|(r, ring)| {
            let n = ring.len();
            let mut s = 0.0;
            for i in 0..n {
                let (a, b) = (ring[i], ring[(i + 1) % n]);
                s += a.x * b.y - b.x * a.y;
            }
            (r.level_m, s.abs() / 2.0)
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v
}

/// Feed the sketch engine a pixel stroke (the close-check input) and apply a
/// 10×10 world trace of the storey below — the upper-storey draw path.
fn draw_world_square_trace(c: &mut AppController) {
    c.sketch_start_stroke(Point2f32::new(0.0, 0.0)).unwrap();
    for (x, y) in [(100.0, 0.0), (100.0, 100.0), (0.0, 100.0), (1.0, 1.0)] {
        c.sketch_add_point(Point2f32::new(x, y)).unwrap();
    }
    let world = vec![(0.2, 0.2), (9.8, 0.2), (9.8, 9.8), (0.2, 9.8), (0.3, 0.3)];
    c.apply_outline_stroke_world(&world);
}

/// THICKNESS-FIRST: `add_storey` opens the flow at the thickness question
/// (never on storey 0); outline strokes are ignored until it lands; invalid
/// values keep the prompt open; a valid value stamps the durable
/// `floor_thickness_m` and advances to the method choice; the Heights
/// re-edit (`set_floor_thickness`) never touches the stage.
#[test]
fn new_storey_thickness_is_asked_first_and_gates_outline_strokes() {
    let mut c = dimensioned_square_controller();
    assert!(c.new_storey_prompt().is_none(), "no flow on storey 0");
    assert_eq!(c.floor_thickness(), None, "storey 0 has no slab thickness");
    assert!(
        c.assign_floor_thickness(0.3).is_err(),
        "storey 0 refuses a thickness"
    );

    c.add_storey().unwrap();
    let prompt = c.new_storey_prompt().expect("flow opened by add_storey");
    assert!(prompt.thickness_pending && !prompt.method_pending);
    assert!(prompt.suggestion.is_none());

    // The world outline stroke is IGNORED while the thickness is pending.
    draw_world_square_trace(&mut c);
    assert_eq!(c.document.floor_plan.ring_count(), 0, "stroke ignored");
    assert!(
        c.new_storey_prompt().unwrap().thickness_pending,
        "still asking for the thickness"
    );

    // Invalid values keep the prompt open and stamp nothing.
    assert!(c.assign_floor_thickness(-0.1).is_err());
    assert!(c.assign_floor_thickness(f64::NAN).is_err());
    assert_eq!(c.floor_thickness(), None);

    c.assign_floor_thickness(0.30).expect("valid thickness");
    assert_eq!(c.floor_thickness(), Some(0.30), "display value");
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[1].floor_thickness_m,
        Some(0.30),
        "durably stamped on the Storey"
    );
    let prompt = c.new_storey_prompt().unwrap();
    assert!(!prompt.thickness_pending && prompt.method_pending);

    // Heights re-edit: value changes, the flow stage doesn't.
    c.set_floor_thickness(0.25).expect("re-edit applies");
    assert_eq!(c.floor_thickness(), Some(0.25));
    assert!(
        c.new_storey_prompt().unwrap().method_pending,
        "set_floor_thickness never advances the stage"
    );
}

/// SUGGEST → CYCLE → ACCEPT: the suggestions derive from the storey below
/// (full-footprint two-level candidate ranked first), the cursor wraps both
/// ways, and accepting adopts a FULLY-FINALISED outline — no dimension
/// prompts, no collinearity review — and seeds the inherited floor level
/// (canonicalized). Undo restores the empty outline; the seeded steps stay
/// (outside undo — trap 6).
#[test]
fn suggested_outline_accepts_finalised_with_no_prompts_or_review() {
    let mut c = stepped_ceiling_below_controller();
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    assert!(
        c.new_storey_prompt().unwrap().can_suggest,
        "below has height + flat ceilings"
    );

    c.choose_suggest_outlines().expect("suggestions derive");
    let (ring, _label, idx0, count) = c.new_storey_prompt().unwrap().suggestion.expect("browsing");
    assert_eq!(idx0, 0);
    assert!(count >= 2, "stepped ceiling yields several candidates");
    // Top-ranked: the full 10×10 footprint (coverage + compactness beat the
    // single-level halves).
    let ring_area = {
        let n = ring.len();
        let mut s = 0.0;
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            s += a.0 * b.1 - b.0 * a.1;
        }
        s.abs() / 2.0
    };
    assert!((ring_area - 100.0).abs() < 1.0, "full footprint first");

    // Cycle wraps forward and back.
    c.cycle_suggestion(true);
    assert_eq!(c.new_storey_prompt().unwrap().suggestion.unwrap().2, 1);
    c.cycle_suggestion(false);
    assert_eq!(c.new_storey_prompt().unwrap().suggestion.unwrap().2, 0);

    c.accept_suggestion().expect("accept");
    assert!(c.new_storey_prompt().is_none(), "flow closed");
    assert!(c.outline_finalised(), "fully finalised outline");
    assert!(c.current_dimension_prompt().is_none(), "no prompts");
    assert!(c.review.is_none(), "no collinearity review");
    assert!(
        !c.inherited_seed_pending.contains(&1),
        "seed consumed at accept"
    );

    // The inherited level landed: storey 1's floor split, the raised half
    // (x ≥ 5, 50 m²) at +0.3, the rest at the datum.
    let levels = floor_level_areas(&c, 1);
    assert_eq!(levels.len(), 2, "floor split at the inherited line");
    assert!((levels[0].0 - 0.0).abs() < 1e-9 && (levels[0].1 - 50.0).abs() < 1e-4);
    assert!((levels[1].0 - 0.3).abs() < 1e-9 && (levels[1].1 - 50.0).abs() < 1e-4);
    // Session list canonicalized to the exact derived piece.
    let session = &c.step_regions[&1];
    assert_eq!(session.len(), 1);
    assert!((session[0].level_m - 0.3).abs() < 1e-9);

    // Undo: the adoption reverts (empty outline again); the seeded steps and
    // the thickness remain — outside undo by design.
    c.undo().unwrap();
    assert_eq!(c.document.floor_plan.ring_count(), 0, "outline undone");
    assert_eq!(c.step_regions[&1].len(), 1, "seeded steps survive undo");
    assert_eq!(c.floor_thickness(), Some(0.3), "thickness survives undo");
}

/// `choose_suggest_outlines` fails with a STABLE error when there's nothing
/// to suggest from (no ceiling height below), and the prompt's `can_suggest`
/// gate agrees.
#[test]
fn choose_suggest_without_flat_ceilings_below_is_a_stable_err() {
    let mut c = dimensioned_square_controller(); // no ceiling height set
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    let prompt = c.new_storey_prompt().unwrap();
    assert!(prompt.method_pending && !prompt.can_suggest);
    let err = c.choose_suggest_outlines().expect_err("nothing to suggest");
    assert!(
        err.contains("Draw the outline instead"),
        "stable, actionable message: {err}"
    );
    assert!(
        err.contains("ceiling height"),
        "the no-inputs cause is named: {err}"
    );
    // The flow stays at the method choice — drawing still works.
    assert!(c.new_storey_prompt().unwrap().method_pending);
}

/// `choose_suggest_outlines` attributes an EMPTY result honestly: when the
/// storey below HAS its ceiling height and a flat ceiling area but every
/// candidate is under the suggestion engine's sliver floor, the error says
/// the areas are TOO SMALL — not that the height / flat area are missing.
#[test]
fn choose_suggest_with_only_sliver_flat_areas_says_too_small() {
    use ber_model::ber::structural::{CeilingRegions, FloorRegions};
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    // Shrink the storey below's resolved geometry to a true sliver
    // (0.6 × 0.6 m = 0.36 m²): the suggestion inputs are present, but every
    // candidate falls under the engine's area floor.
    {
        let record = c.current_record.as_mut().unwrap();
        let below = &mut record.data.storeys[0];
        let ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(0.6, 0.0),
            Point2f64::new(0.6, 0.6),
            Point2f64::new(0.0, 0.6),
        ];
        below.floors = FloorRegions::new(ring.clone(), Default::default());
        below.ceilings = CeilingRegions::default_for_footprint(vec![ring], 2.4, Default::default());
    }
    assert!(
        c.new_storey_prompt().unwrap().can_suggest,
        "the cheap input gate still holds — only the area floor filters"
    );
    let err = c.choose_suggest_outlines().expect_err("all filtered");
    assert!(err.contains("too small"), "honest cause: {err}");
    assert!(
        err.contains("Draw the outline instead"),
        "still actionable: {err}"
    );
    // The flow stays at the method choice — drawing still works.
    assert!(c.new_storey_prompt().unwrap().method_pending);
}

/// DRAWN PATH: a canvas stroke at the method choice is the implicit
/// choose_draw_outline; the fully-pinned trace finalises at the carve tail,
/// which seeds the inherited floor level right there.
#[test]
fn drawn_outline_seeds_inherited_levels_at_finalisation() {
    let mut c = stepped_ceiling_below_controller();
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    assert!(c.new_storey_prompt().unwrap().method_pending);

    // The stroke IS the draw choice — flow closes, carve applies.
    draw_world_square_trace(&mut c);
    assert!(
        c.new_storey_prompt().is_none(),
        "stroke chose the draw path"
    );
    assert_eq!(c.document.floor_plan.ring_count(), 1, "outline carved");
    assert!(c.outline_finalised(), "fully pinned trace → finalised");
    assert!(!c.inherited_seed_pending.contains(&1), "seed consumed");

    let session = c.step_regions.get(&1).expect("inherited level seeded");
    assert_eq!(session.len(), 1);
    assert!((session[0].level_m - 0.3).abs() < 1e-9);
    c.materialize();
    let levels = floor_level_areas(&c, 1);
    assert_eq!(levels.len(), 2, "storey 1's floor split");
    assert!(levels
        .iter()
        .any(|(l, a)| (l - 0.3).abs() < 1e-9 && (a - 50.0).abs() < 1.0));
}

/// DRAWN PATH ACROSS A RESTART: the storey is added but not drawn, the app
/// "restarts" (flush + open_ber), and the re-derived seed-pending set still
/// fires the seed when the outline finalises. The un-finalised persist kept
/// the thickness (trap 7); finalised storeys never re-pend (trap 3).
#[test]
fn drawn_path_seed_survives_restart_via_rederived_pending() {
    let mut c = stepped_ceiling_below_controller();
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).expect("reopen");
    assert!(
        c.inherited_seed_pending.contains(&1),
        "un-finalised upper storey re-pends on open"
    );
    assert!(
        !c.inherited_seed_pending.contains(&0),
        "storey 0 never pends"
    );
    assert!(
        c.new_storey_prompt().is_none(),
        "the flow itself is not restored — only the seed transition"
    );
    // The snapshot resumed on the storey being edited (1); the un-finalised
    // persist kept the thickness.
    assert_eq!(c.current_storey_idx(), Some(1));
    assert_eq!(c.floor_thickness(), Some(0.3), "thickness survived restart");

    draw_world_square_trace(&mut c);
    assert!(c.outline_finalised());
    assert!(!c.inherited_seed_pending.contains(&1), "seed consumed");
    let session = c.step_regions.get(&1).expect("seeded after the restart");
    assert!((session[0].level_m - 0.3).abs() < 1e-9);
    c.materialize();
    assert!(
        floor_level_areas(&c, 1)
            .iter()
            .any(|(l, _)| (l - 0.3).abs() < 1e-9),
        "inherited level derived into the floors"
    );
}

/// IN-SESSION REDRAW RE-SEEDS: finalise an upper storey over the stepped
/// below-ceiling (the seed fires, canonicalizing pieces against THAT
/// footprint), then redraw the outline wholesale via the Replace path onto
/// the raised half only. The redraw must re-PEND the seed and DROP the stale
/// seeded pieces (they were canonicalized against the old footprint — under
/// the old behaviour they'd graft a bogus +0.3 step onto the new floor);
/// re-dimensioning to finalisation then re-seeds against the NEW footprint:
/// sitting entirely on the 2.7 ceiling, its anchor IS that level, so the
/// floor is flat — matching what a restart's re-derive would produce.
#[test]
fn upper_storey_redraw_reseeds_inherited_levels_against_new_footprint() {
    let mut c = stepped_ceiling_below_controller();
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();

    // First outline: the full 10×10 trace — finalised at the carve tail, the
    // seed fires: one inherited piece (x ≥ 5, +0.3).
    draw_world_square_trace(&mut c);
    assert!(c.outline_finalised(), "fully pinned trace → finalised");
    assert!(!c.inherited_seed_pending.contains(&1), "seed consumed");
    assert_eq!(c.step_regions[&1].len(), 1, "inherited piece seeded");

    // Wholesale REDRAW via the Replace path: a pixel rect that will be
    // dimensioned onto the raised right half only (x ∈ [5,10], y ∈ [0,10]).
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    c.set_edit_mode(EditTools::DrawPerpWalls);
    c.sketch_start_stroke(p(500.0, 0.0)).unwrap();
    // Tail anatomy: (500,50) is the drag point homing in along the closing
    // edge (outside the 30 px close zone, so `check_closed` keeps it and the
    // orthogonalizer pops it as the drag point); (501,1) is the lift point
    // inside the zone, which `check_closed` pops.
    for (x, y) in [
        (1000.0, 0.0),
        (1000.0, 1000.0),
        (500.0, 1000.0),
        (500.0, 50.0),
        (501.0, 1.0),
    ] {
        c.sketch_add_point(p(x, y)).unwrap();
    }
    c.sketch_end_stroke(p(501.0, 1.0)).unwrap();
    assert!(c.has_pending_redraw(), "existing plan → redraw decision");
    c.confirm_replace_storey().expect("replace applies");

    // The replacement re-pended the seed and dropped the stale seeded pieces
    // — in-session now matches what a restart's re-derive would do.
    assert!(
        c.inherited_seed_pending.contains(&1),
        "wholesale redraw re-pends the inherited seed"
    );
    assert!(
        c.step_regions.get(&1).is_none(),
        "stale seeded pieces die with the old footprint"
    );

    // Re-dimension the redrawn rect to finalisation: verticals 10 m,
    // horizontals 5 m (true lengths, so the geometry lands at x ∈ [5,10]).
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        let prompt = c.document.dimensioning.current_prompt.clone().unwrap();
        let (_, a, b) = c
            .document
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .find(|(vid, _, _)| *vid == prompt.wall_start_vertex)
            .expect("prompted wall exists");
        let v = if (a.x - b.x).abs() < (a.y - b.y).abs() {
            10.0
        } else {
            5.0
        };
        c.apply_dimension(v).expect("dimension applies");
        guard += 1;
    }
    assert!(guard < 12, "dimension loop didn't terminate");
    assert!(c.outline_finalised(), "redraw finalised");
    assert!(
        !c.inherited_seed_pending.contains(&1),
        "seed re-fired at the redraw's finalisation"
    );

    // Seeded against the NEW footprint: it sits entirely on the 2.7 ceiling,
    // so that level is the anchor and NOTHING stands proud — a flat floor.
    // (The stale graft would instead have raised the whole floor to +0.3.)
    assert!(
        c.step_regions.get(&1).is_none_or(|v| v.is_empty()),
        "no inherited steps on a single-level footprint: {:?}",
        c.step_regions.get(&1)
    );
    c.materialize();
    let levels = floor_level_areas(&c, 1);
    assert_eq!(levels.len(), 1, "one flat floor region: {levels:?}");
    assert!(
        levels[0].0.abs() < 1e-9 && (levels[0].1 - 50.0).abs() < 1e-3,
        "flat 50 m² floor at the datum, not the old +0.3 graft: {levels:?}"
    );
}

/// LEGACY STACKING UNTOUCHED: a storey added without ever assigning a
/// thickness keeps `floor_thickness_m = None`, and the datum stack is the
/// byte-identical ceiling-height sum.
#[test]
fn legacy_none_thickness_keeps_ceiling_stacking_datums() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    // The assessor never enters a thickness (old workflow): dismiss the flow
    // and draw directly.
    c.choose_draw_outline();
    finalise_current_storey_as_square(&mut c, 6.0);
    let ber = c.current_record.as_ref().unwrap().data.clone();
    assert_eq!(ber.storeys[1].floor_thickness_m, None, "never stamped");
    let datums = ber.storey_floor_datums();
    let height = ber.storeys[0].ceiling_height_m.unwrap();
    assert_eq!(datums.len(), 2);
    assert_eq!(datums[0], 0.0);
    assert_eq!(
        datums[1], height,
        "legacy stacking: BYTE-IDENTICAL to the below-ceiling height sum"
    );
}

/// CONTROLLER-LEVEL FIXPOINT: accept a suggestion (seeded + canonicalized),
/// reload, and a no-edit re-derive reproduces the identical floor split —
/// the seeded pieces equal what the reload reconstructs (trap 1).
#[test]
fn accepted_suggestion_split_survives_reload_and_rederive() {
    let mut c = stepped_ceiling_below_controller();
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    c.choose_suggest_outlines().unwrap();
    c.accept_suggestion().unwrap();
    c.materialize();
    let before = floor_level_areas(&c, 1);
    assert_eq!(before.len(), 2);

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).expect("reopen");
    assert!(
        !c.inherited_seed_pending.contains(&1),
        "finalised storey never retro-seeds"
    );
    // Session step regions rebuilt from the persisted levelled floors.
    let session = c.step_regions.get(&1).expect("reconstructed on open");
    assert_eq!(session.len(), 1);
    assert!((session[0].level_m - 0.3).abs() < 1e-9);

    // Re-derive without any edit → identical split.
    c.mark_dirty().unwrap();
    c.materialize();
    let after = floor_level_areas(&c, 1);
    assert_eq!(before.len(), after.len());
    for ((bl, ba), (al, aa)) in before.iter().zip(&after) {
        assert!(
            (bl - al).abs() < 1e-9 && (ba - aa).abs() < 1e-4,
            "floor fingerprint changed: ({bl}, {ba}) vs ({al}, {aa})"
        );
    }
}

/// THE THICKNESS PERSIST LEG (the ridge/rect persist tests' shape, slab
/// flavour): a finalised upper storey with `floor_thickness_m` set drops its
/// perimeter wall bases by the slab at resolve — persist → reload →
/// re-resolve must reproduce the wall regions BYTE-IDENTICALLY (the
/// float_roundtrip JSON guarantee), with the thickness itself surviving the
/// restart.
#[test]
fn floor_thickness_wall_regions_survive_reload_and_rederive_fixpoint() {
    let mut c = stepped_ceiling_below_controller();
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    c.choose_suggest_outlines().unwrap();
    c.accept_suggestion().unwrap();
    c.materialize();
    let walls_json = |c: &AppController| {
        c.current_record.as_ref().unwrap().data.storeys[1]
            .walls
            .regions
            .iter()
            .map(|r| serde_json::to_string(&r.polygon).unwrap())
            .collect::<Vec<_>>()
            .join("|")
    };
    let before = walls_json(&c);
    assert!(
        !c.current_record.as_ref().unwrap().data.storeys[1]
            .walls
            .regions
            .is_empty(),
        "the finalised upper storey derives wall regions"
    );

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).expect("reopen");
    assert_eq!(c.floor_thickness(), Some(0.3), "thickness survived reload");
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        walls_json(&c),
        before,
        "resolve → persist → reload → re-resolve keeps the slab-dropped \
         wall regions byte-identical"
    );
}

// ── Windows / doors placement (openings tool) ───────────────────────────────

/// Placing a window via the openings tool: pick the nearest wall, enter size +
/// glazing, and it lands with derived vertices, a registered window type, and an
/// `opes` link — and UNDO removes it (BER-side state is in the snapshot now).
#[test]
fn place_window_via_controller_persists_and_undoes() {
    use ber_model::ber::fenestration::{Frame, Glazing};
    let mut c = dimensioned_square_controller();
    c.materialize();
    c.set_openings_mode(true);
    c.set_openings_tool(OpeningToolKind::Window);
    // Tap 0.2 m outside the bottom wall, at x = 5 → picks that wall.
    c.openings_pointer_down(5.0, -0.2, 0.5);
    assert!(c.pending_opening_prompt().is_some(), "a wall was picked");
    c.place_pending_window(1.0, 1.2, 1.0, Glazing::Double, Frame::WoodUPVC, 2010)
        .expect("window places");
    c.materialize();
    {
        let ber = &c.current_record.as_ref().unwrap().data;
        assert_eq!(ber.storeys[0].windows.len(), 1, "window added");
        assert_eq!(
            ber.storeys[0].windows[0].vertices.len(),
            4,
            "vertices derived"
        );
        assert_eq!(ber.window_types.len(), 1, "window type registered");
        assert!(
            ber.storeys[0]
                .walls
                .regions
                .iter()
                .any(|r| !r.opes.windows.is_empty()),
            "opes linked"
        );
    }
    c.undo().expect("undo");
    c.materialize();
    assert!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .windows
            .is_empty(),
        "undo removed the window"
    );
}

/// Regression (window/opening placement): the wall + ceiling region ids re-mint
/// on every resolve, so if a revision bump lands BETWEEN the tap (host pick) and
/// the confirm — e.g. attaching a photo in the details box calls `mark_dirty` —
/// the confirm's `materialize` re-resolves and the tap-time `wall_id` dangles.
/// The pending WALL target now carries a durable anchor and re-derives the live
/// id at confirm; before the fix this errored `NoHostRegion`.
#[test]
fn wall_window_survives_id_remint_between_tap_and_confirm() {
    use super::openings::PendingOpeningTarget;
    use ber_model::ber::fenestration::{Frame, Glazing, OpeningPlacement};
    let mut c = dimensioned_square_controller();
    c.materialize();
    c.select_opening_tool(OpeningToolKind::Window);
    // Tap 0.2 m outside the bottom wall (y=0) at x=5 → picks it + captures the
    // durable parametric anchor.
    c.openings_pointer_down(5.0, -0.2, 0.5);
    assert!(c.pending_opening_prompt().is_some(), "a wall was picked");
    match c.pending_opening.as_ref().map(|p| p.target) {
        Some(PendingOpeningTarget::Wall { anchor, .. }) => {
            assert!(anchor.is_some(), "the tap captured a durable wall anchor")
        }
        other => panic!("expected a pending wall target, got {other:?}"),
    }
    // Simulate attaching a photo in the details box: bumps the revision so the
    // confirm's `materialize` re-resolves and RE-MINTS every wall id.
    c.mark_dirty().unwrap();
    // Confirm: without the anchor re-home this errors NoHostRegion.
    c.place_pending_window(1.0, 1.2, 1.0, Glazing::Double, Frame::WoodUPVC, 2010)
        .expect("window still places on the re-minted wall");
    c.materialize();
    let ber = &c.current_record.as_ref().unwrap().data;
    assert_eq!(ber.storeys[0].windows.len(), 1, "window added");
    // It landed on a LIVE wall region (its host id is present) which is the
    // bottom edge (y≈0), and the wall's opes link back to it.
    let host = match &ber.storeys[0].windows[0].placement {
        Some(OpeningPlacement::Wall { wall_id, .. }) => *wall_id,
        other => panic!("expected a wall placement, got {other:?}"),
    };
    let wall = ber.storeys[0]
        .walls
        .regions
        .iter()
        .find(|r| r.id == host)
        .expect("placement points at a LIVE wall region");
    assert!(
        wall.opes.windows.contains(&ber.storeys[0].windows[0].id),
        "opes linked to the live wall"
    );
    let (origin, _, _, _) = wall.base_frame().unwrap();
    assert!(
        origin.y.abs() < 0.2,
        "landed on the bottom wall (y≈0), got y={}",
        origin.y
    );
}

/// Roof twin of the above: a roof window's pending target stores the world plan
/// point, so a revision bump between tap and confirm (photo attach → re-mint of
/// ceiling region ids) is survived by re-running the region pick from that point
/// at confirm instead of trusting the tap-time `region_id`.
#[test]
fn roof_window_survives_id_remint_between_tap_and_confirm() {
    use ber_model::ber::fenestration::{Frame, Glazing, OpeningPlacement};
    let mut c = dimensioned_square_controller();
    // The roof (ceiling) region only resolves once a ceiling height is set.
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();
    c.select_opening_tool(OpeningToolKind::RoofWindow);
    // Tap inside the ceiling region (centre of the 10×10 storey).
    c.openings_pointer_down(5.0, 5.0, 0.5);
    assert!(
        c.pending_opening_prompt().is_some(),
        "a roof region was picked"
    );
    // Re-mint the ceiling region ids under the pending target.
    c.mark_dirty().unwrap();
    c.place_pending_window(1.0, 1.0, 0.0, Glazing::Double, Frame::WoodUPVC, 2010)
        .expect("roof window still places on the re-minted region");
    c.materialize();
    let ber = &c.current_record.as_ref().unwrap().data;
    assert_eq!(ber.storeys[0].windows.len(), 1, "roof window added");
    let region = match &ber.storeys[0].windows[0].placement {
        Some(OpeningPlacement::Roof {
            ceiling_region_id, ..
        }) => *ceiling_region_id,
        other => panic!("expected a roof placement, got {other:?}"),
    };
    assert!(
        ber.storeys[0]
            .ceilings
            .regions
            .iter()
            .any(|r| r.id == region),
        "placement points at a LIVE ceiling region"
    );
}

/// The multi-field placement (toolbar tool → tap → floating box with Width +
/// Height): selecting the tool enters openings mode, the tap stages the host and
/// asks for both measurements at once, and `place_opening` lands a window with a
/// DEFAULT window type + DEFAULT 1 m sill (both refined later — type in Edit on
/// surface, sill in the 2D view).
#[test]
fn multi_field_window_places_with_default_sill() {
    let mut c = dimensioned_square_controller();
    c.materialize();
    // One toolbar action both enters openings mode and selects the tool.
    c.select_opening_tool(OpeningToolKind::Window);
    assert!(c.openings_mode());
    c.openings_pointer_down(5.0, -0.2, 0.5);
    // The floating box asks for Width + Height (two fields, both required) — no
    // sill field.
    let p = c.opening_fields_prompt().expect("placement staged");
    assert_eq!(p.fields.len(), 2, "width + height only (no sill)");
    assert!(p.fields.iter().all(|f| f.required));
    // Width + height entered together; sill is NOT supplied (defaults to 1 m).
    c.place_opening(
        1.0,
        1.2,
        None,
        None,
        None,
        None,
        None,
        false,
        WindowTypeChoice::default(),
        ber_model::ber::fenestration::Overshading::Average,
        Vec::new(),
        PhotoQuality::High,
    )
    .expect("window places");
    assert!(c.opening_fields_prompt().is_none(), "placement finished");
    c.materialize();
    let ber = &c.current_record.as_ref().unwrap().data;
    assert_eq!(ber.storeys[0].windows.len(), 1, "window placed");
    assert_eq!(ber.window_types.len(), 1, "default window type registered");
    assert!(!ber.storeys[0].windows[0].multi_storey, "plain window");
    // The default 1 m sill landed on the placement.
    match &ber.storeys[0].windows[0].placement {
        Some(ber_model::ber::fenestration::OpeningPlacement::Wall { sill_m, .. }) => {
            assert!(
                (sill_m - 1.0).abs() < 1e-4,
                "default 1 m sill, got {sill_m}"
            );
        }
        other => panic!("expected a wall placement, got {other:?}"),
    }
    // Undo removes it (BER-side state is snapshotted).
    c.undo().expect("undo");
    c.materialize();
    assert!(c.current_record.as_ref().unwrap().data.storeys[0]
        .windows
        .is_empty());
}

/// The window-type picker shares a single shared `WindowType` across windows of
/// the same (glazing, frame, year, certified) — and only mints a new type when
/// they differ. Mirrors the "assign to existing / only add if different" rule.
#[test]
fn window_type_picker_reuses_existing_and_adds_only_when_different() {
    use ber_model::ber::fenestration::{Frame, Glazing, Overshading};

    let mut c = dimensioned_square_controller();
    c.materialize();

    // Helper: stage a placement on the south wall and place with a given choice.
    let place = |c: &mut AppController, choice: WindowTypeChoice| {
        c.select_opening_tool(OpeningToolKind::Window);
        c.openings_pointer_down(5.0, -0.2, 0.5);
        c.place_opening(
            1.0,
            1.2,
            None,
            None,
            None,
            None,
            None,
            false,
            choice,
            Overshading::Average,
            Vec::new(),
            PhotoQuality::High,
        )
        .expect("window places");
        c.materialize();
    };

    // 1) First NEW double / Wood-UPVC / 2010 / uncertified type.
    place(
        &mut c,
        WindowTypeChoice::New {
            glazing: Glazing::Double,
            frame: Frame::WoodUPVC,
            year: 2010,
            certified: false,
            gap: Vec::new(),
            name: None,
        },
    );
    assert_eq!(
        c.current_record.as_ref().unwrap().data.window_types.len(),
        1,
        "first type registered"
    );

    // 2) An IDENTICAL New choice joins the same type — no new type added.
    place(
        &mut c,
        WindowTypeChoice::New {
            glazing: Glazing::Double,
            frame: Frame::WoodUPVC,
            year: 2010,
            certified: false,
            gap: Vec::new(),
            name: None,
        },
    );
    let ber = &c.current_record.as_ref().unwrap().data;
    assert_eq!(ber.window_types.len(), 1, "identical attrs reuse the type");
    assert_eq!(
        ber.storeys[0].windows[0].window_type_id, ber.storeys[0].windows[1].window_type_id,
        "both windows share one type id"
    );

    // 3) Same attrs but CERTIFIED → a distinct type (cert is part of identity).
    place(
        &mut c,
        WindowTypeChoice::New {
            glazing: Glazing::Double,
            frame: Frame::WoodUPVC,
            year: 2010,
            certified: true,
            gap: Vec::new(),
            name: None,
        },
    );
    assert_eq!(
        c.current_record.as_ref().unwrap().data.window_types.len(),
        2,
        "certified variant is a new type"
    );

    // 3b) Same attrs but a different NAME (model) → a distinct type.
    place(
        &mut c,
        WindowTypeChoice::New {
            glazing: Glazing::Double,
            frame: Frame::WoodUPVC,
            year: 2010,
            certified: false,
            gap: Vec::new(),
            name: Some("Model B".to_string()),
        },
    );
    assert_eq!(
        c.current_record.as_ref().unwrap().data.window_types.len(),
        3,
        "a named model is a new type even with identical attrs"
    );

    // 3c) Same attrs but a different GAP → a distinct type.
    place(
        &mut c,
        WindowTypeChoice::New {
            glazing: Glazing::Double,
            frame: Frame::WoodUPVC,
            year: 2010,
            certified: false,
            gap: vec![16],
            name: None,
        },
    );
    assert_eq!(
        c.current_record.as_ref().unwrap().data.window_types.len(),
        4,
        "a different gap is a new type"
    );

    // 4) Existing types surface for the picker; reusing one by id adds none.
    let existing = c.existing_window_types();
    assert_eq!(existing.len(), 4, "all four types in use are offered");
    let reuse_id = existing[0].id;
    place(&mut c, WindowTypeChoice::Existing(reuse_id));
    let ber = &c.current_record.as_ref().unwrap().data;
    assert_eq!(
        ber.window_types.len(),
        4,
        "reusing an existing adds no type"
    );
    assert_eq!(
        ber.storeys[0].windows.last().unwrap().window_type_id,
        reuse_id,
        "the placed window joined the picked existing type"
    );
}

/// Overshading is a PER-WINDOW input: the placement box's choice lands on the
/// placed window, and defaults to Average — two windows can differ.
#[test]
fn overshading_is_applied_per_window_at_placement() {
    use ber_model::ber::fenestration::Overshading;

    let mut c = dimensioned_square_controller();
    c.materialize();

    let place = |c: &mut AppController, overshading: Overshading| {
        c.select_opening_tool(OpeningToolKind::Window);
        c.openings_pointer_down(5.0, -0.2, 0.5);
        c.place_opening(
            1.0,
            1.2,
            None,
            None,
            None,
            None,
            None,
            false,
            WindowTypeChoice::default(),
            overshading,
            Vec::new(),
            PhotoQuality::High,
        )
        .expect("window places");
        c.materialize();
    };

    place(&mut c, Overshading::Heavy);
    place(&mut c, Overshading::VeryLittle);
    let windows = &c.current_record.as_ref().unwrap().data.storeys[0].windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(
        windows[0].overshading,
        Overshading::Heavy,
        "first window keeps its overshading"
    );
    assert_eq!(
        windows[1].overshading,
        Overshading::VeryLittle,
        "second window has its own (per-window, not shared)"
    );
}

/// A photo picked in the placement box is copied into the store and attached to
/// the placed window's TYPE (group) — photos belong to the group, shared by all
/// its windows, not the individual window.
#[test]
fn window_photo_is_attached_at_placement() {
    use ber_model::ber::fenestration::Overshading;

    let mut c = dimensioned_square_controller();
    c.materialize();
    let tmp = std::env::temp_dir().join(format!("ber_photos_{}", ItemId::default().0));
    c.set_photos_dir(tmp);
    let src = std::env::temp_dir().join(format!("ber_winsrc_{}.jpg", ItemId::default().0));
    std::fs::write(&src, b"jpeg-bytes").unwrap();

    c.select_opening_tool(OpeningToolKind::Window);
    c.openings_pointer_down(5.0, -0.2, 0.5);
    c.place_opening(
        1.0,
        1.2,
        None,
        None,
        None,
        None,
        None,
        false,
        WindowTypeChoice::New {
            glazing: ber_model::ber::fenestration::Glazing::Double,
            frame: ber_model::ber::fenestration::Frame::WoodUPVC,
            year: 0,
            certified: false,
            gap: Vec::new(),
            name: Some("Model A".to_string()),
        },
        Overshading::Average,
        vec![src],
        PhotoQuality::Full,
    )
    .expect("window places");
    c.materialize();

    let ber = &c.current_record.as_ref().unwrap().data;
    assert_eq!(ber.storeys[0].windows.len(), 1);
    // The photo lives on the window's TYPE (the shared group), not the window.
    let type_id = ber.storeys[0].windows[0].window_type_id;
    let wt = ber
        .window_types
        .iter()
        .find(|t| t.id == type_id)
        .expect("window type registered");
    assert_eq!(
        wt.photos.len(),
        1,
        "photo attached to the window type (group)"
    );
    assert!(
        std::path::Path::new(&wt.photos[0].file_path).exists(),
        "photo copied into the store"
    );
}

/// A FAILED placement (wall too short) must not leave its picked photos copied
/// in the store — else each retry would leak orphaned image files.
#[test]
fn failed_placement_discards_copied_photos() {
    use ber_model::ber::fenestration::{Frame, Glazing, Overshading};

    let mut c = dimensioned_square_controller();
    c.materialize();
    let tmp = std::env::temp_dir().join(format!("ber_photos_{}", ItemId::default().0));
    c.set_photos_dir(tmp.clone());
    let src = std::env::temp_dir().join(format!("ber_failsrc_{}.jpg", ItemId::default().0));
    std::fs::write(&src, b"jpeg-bytes").unwrap();

    c.select_opening_tool(OpeningToolKind::Window);
    c.openings_pointer_down(5.0, -0.2, 0.5);
    // A window far wider than the wall → WallTooShort → placement fails.
    let r = c.place_opening(
        999.0,
        1.2,
        None,
        None,
        None,
        None,
        None,
        false,
        WindowTypeChoice::New {
            glazing: Glazing::Double,
            frame: Frame::WoodUPVC,
            year: 0,
            certified: false,
            gap: Vec::new(),
            name: Some("Model A".to_string()),
        },
        Overshading::Average,
        vec![src],
        PhotoQuality::Full,
    );
    assert!(r.is_err(), "oversized window fails to place");
    c.materialize();
    assert!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .windows
            .is_empty(),
        "no window placed"
    );
    // Photo ingest is async: the failed placement discards the provisional copy
    // immediately, and the background job — finding its photo gone from the model
    // — cleans up its own produced files when `apply_finished_photos` next runs.
    let mut spins = 0;
    while c.photos_processing() && spins < 2000 {
        std::thread::sleep(std::time::Duration::from_millis(2));
        spins += 1;
    }
    c.apply_finished_photos();
    let leaked = std::fs::read_dir(&tmp).map(|d| d.count()).unwrap_or(0);
    assert_eq!(
        leaked, 0,
        "no orphaned photo files after a failed placement"
    );
}

/// A placed window survives save + reload: the placement is durable, and the
/// reload re-derives vertices and re-links `opes` via the amend.
#[test]
fn placed_window_survives_reload() {
    use ber_model::ber::fenestration::{Frame, Glazing};
    let mut c = dimensioned_square_controller();
    c.materialize();
    c.set_openings_mode(true);
    c.openings_pointer_down(5.0, -0.2, 0.5);
    c.place_pending_window(1.0, 1.2, 1.0, Glazing::Triple, Frame::Metal, 2005)
        .expect("window places");
    c.materialize();
    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.windows.len(), 1, "window persisted across reload");
    assert_eq!(
        storey.windows[0].vertices.len(),
        4,
        "vertices re-derived on reload"
    );
    assert!(
        storey
            .walls
            .regions
            .iter()
            .any(|r| !r.opes.windows.is_empty()),
        "opes re-linked across reload"
    );
}

/// Door+sidelights composer (via the controller): one tap + a spec yields one
/// door panel plus its sidelight/transom windows, all in one assembly.
#[test]
fn place_door_sidelights_via_controller() {
    use ber_model::ber::fenestration::{Frame, Glazing};
    let mut c = dimensioned_square_controller();
    c.materialize();
    c.set_openings_mode(true);
    c.set_openings_tool(OpeningToolKind::DoorSidelights);
    c.openings_pointer_down(5.0, -0.2, 0.5);
    c.place_pending_door_sidelights(
        0.9,
        2.0,
        Some(0.4),
        Some(0.4),
        Some(0.4),
        Glazing::Double,
        Frame::WoodUPVC,
        2010,
    )
    .expect("assembly places");
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.doors.len(), 1, "one door panel");
    assert_eq!(storey.windows.len(), 3, "two sidelights + a transom");
    let aid = storey.doors[0].assembly_id;
    assert!(aid.is_some(), "door carries an assembly id");
    assert!(
        storey.windows.iter().all(|w| w.assembly_id == aid),
        "every glass member shares the assembly"
    );
}

/// Elevation editor: open a wall's elevation, drag an opening (no overlap), and
/// delete the selected one.
#[test]
fn elevation_move_and_delete_openings() {
    use ber_model::ber::fenestration::{Frame, Glazing};
    let mut c = dimensioned_square_controller();
    c.materialize();
    c.set_openings_mode(true);
    // Two windows on the bottom wall.
    c.openings_pointer_down(2.0, -0.2, 0.5);
    c.place_pending_window(1.0, 1.2, 1.0, Glazing::Double, Frame::WoodUPVC, 2010)
        .unwrap();
    c.openings_pointer_down(8.0, -0.2, 0.5);
    c.place_pending_window(1.0, 1.2, 1.0, Glazing::Double, Frame::WoodUPVC, 2010)
        .unwrap();
    c.materialize();
    // Open the bottom wall's elevation (Edit tap).
    c.set_openings_tool(OpeningToolKind::Edit);
    c.openings_pointer_down(5.0, -0.2, 0.5);
    assert!(c.elevation_wall().is_some(), "elevation opened");
    let ev = c.elevation_view().expect("an elevation view");
    assert_eq!(ev.openings.len(), 2, "both windows shown on the wall");
    // Grab the first opening at its centre, drag it, release.
    let o0 = &ev.openings[0];
    let (a0, mid_h) = (o0.along_m, o0.sill_m + o0.height_m / 2.0);
    c.elevation_pointer_down(a0, mid_h);
    c.elevation_pointer_drag(a0 + 1.0, mid_h);
    c.elevation_pointer_up();
    c.materialize();
    // The two still don't overlap (redistribution held).
    let ev = c.elevation_view().unwrap();
    let mut spans: Vec<(f32, f32)> = ev
        .openings
        .iter()
        .map(|o| (o.along_m - o.width_m / 2.0, o.along_m + o.width_m / 2.0))
        .collect();
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    assert!(spans[1].0 >= spans[0].1 - 1e-3, "no overlap after the drag");
    // Delete the selected (dragged) opening.
    c.delete_selected_opening();
    c.materialize();
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .windows
            .len(),
        1,
        "the dragged window was deleted"
    );
}

/// Structure-phase FLOOR split: in Carve mode the assessor draws an approximate
/// interior region and DIMENSIONS it — which STAGES the carve as a pending
/// selection, changing NO geometry. Only "Edit attributes" commits it: then the
/// floor splits at the dimensioned ring (durable `Storey.floor_splits`, replayed
/// by `resolve_storey`), one region becomes two, and the piece auto-selects.
#[test]
fn structure_floor_carve_dimensions_then_splits_and_selects() {
    let mut c = dimensioned_square_controller();
    c.materialize();
    assert_eq!(
        c.current_surface_counts().floors,
        1,
        "the 10x10 storey starts with a single floor region"
    );

    // Carve mode on Floors (no phase switch needed — the carve handler only
    // needs a Structure selection).
    set_only_kind(&mut c, SurfaceKind::Floors);
    c.set_structure_select_mode(Some(SelectMode::Carve));

    // An interior 4x4 region at (2,2)-(6,6): a carve (not a straight cut), so it
    // becomes a dimensionable split shape straight away.
    c.apply_structure_carve_stroke(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)]);
    assert!(
        c.dimensioning_active(),
        "the split shape prompts to be dimensioned, not split immediately"
    );
    assert!(
        c.structure_selected_ids().is_empty(),
        "nothing selected until the split commits"
    );

    // Dimension every drawn wall, then accept the (interior) position prompts.
    complete_interior_length_loop(&mut c);
    skip_position_prompts(&mut c);

    // DEFERRED: the dimensioned ring is STAGED, not split — geometry is unchanged
    // until "Edit attributes", so a carve the assessor reconsiders costs nothing.
    assert_eq!(
        c.current_surface_counts().floors,
        1,
        "still one floor — the split is deferred to Edit attributes"
    );
    assert_eq!(
        c.pending_structure_carve_polygons().len(),
        1,
        "the carve is staged as a pending selection"
    );
    assert!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .floor_splits
            .is_empty(),
        "nothing durable written yet"
    );
    // The in-flight split shape is gone — back on the outline.
    assert_eq!(c.document.active, ActiveShape::Outline);
    assert!(!c.structure_cut_pending());

    // Opening "Edit attributes" commits the staged carve: NOW the floor splits in
    // two, the durable field carries the ring, and the chosen piece is selected.
    c.begin_structure_edit();
    assert_eq!(
        c.current_surface_counts().floors,
        2,
        "the staged ring split the floor into two regions"
    );
    assert!(
        !c.structure_selected_ids().is_empty(),
        "the chosen piece's floor auto-selected: {:?}",
        c.structure_selected_ids()
    );
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .floor_splits
            .len(),
        1,
        "the split is stored durably on the storey"
    );
    assert!(
        c.pending_structure_carve_polygons().is_empty(),
        "the staged carve was consumed"
    );
}

/// Structure-phase WALL split via a straight CUT: the cut splits the storey
/// into two candidate slots; the assessor taps one, dimensions it, and the
/// dimensioned cut line is stored as a durable `wall_splits` SEGMENT (the
/// interior cut edge), replayed by `resolve_storey`. Then the flow returns to
/// the outline. (Whether the cut re-resolves to MORE wall regions is the
/// ber_model contract — a full-width bisecting cut runs parallel between two
/// opposite walls, grazing them only at its endpoints, so it doesn't split
/// them; that geometry is exercised in `document`/`ber_model`.)
#[test]
fn structure_wall_cut_picks_dimensions_then_stores_a_durable_segment() {
    let mut c = dimensioned_square_controller();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Walls);
    c.set_structure_select_mode(Some(SelectMode::Carve));

    // A straight cut across the storey (left wall to right wall at y=5) splits
    // it into two candidate regions to pick from.
    c.apply_structure_carve_stroke(&[(0.0, 5.0), (10.0, 5.0)]);
    assert!(
        c.structure_cut_pending(),
        "a straight cut offers two candidate regions to pick"
    );
    assert_eq!(c.structure_cut_regions().len(), 2, "two candidates");

    // Pick the lower piece → it becomes the in-flight split shape (dimensioned,
    // not split immediately).
    c.choose_structure_cut_region(5.0, 2.0);
    assert!(!c.structure_cut_pending(), "pick consumed the candidates");
    assert!(
        c.dimensioning_active() || c.document.dimensioning.current_position_prompt.is_some(),
        "the picked piece prompts to be dimensioned"
    );

    // Accept the drawn cut (skip the length prompts so it stays at y=5) +
    // accept positions; the cut is STAGED on the last skip.
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        c.skip_current_dimension();
        guard += 1;
    }
    skip_position_prompts(&mut c);

    // DEFERRED: nothing durable written yet — the cut is staged as a pending
    // selection until "Edit attributes".
    assert!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .wall_splits
            .is_empty(),
        "no wall split until Edit attributes commits the staged cut"
    );
    assert_eq!(
        c.pending_structure_carve_polygons().len(),
        1,
        "the cut is staged as a pending selection"
    );
    // The in-flight split shape is gone — back on the outline.
    assert_eq!(c.document.active, ActiveShape::Outline);
    assert!(!c.structure_cut_pending());

    // Opening "Edit attributes" commits it: walls store a SEGMENT (not a ring),
    // durably, replayed on re-resolve.
    c.begin_structure_edit();
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .wall_splits
            .len(),
        1,
        "the interior cut edge is stored as a durable wall-split segment"
    );
    assert!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .floor_splits
            .is_empty(),
        "a wall split must NOT touch floor_splits"
    );

    // And the split actually took effect: the cut runs left-wall→right-wall,
    // landing at the MIDSPAN of each (cut endpoint = wall t≈0.5), so BOTH the
    // left and right walls divide in two — 4 perimeter walls become 6.
    c.materialize();
    let n_walls = c.current_record.as_ref().unwrap().data.storeys[0]
        .walls
        .regions
        .len();
    assert_eq!(n_walls, 6, "the cut split the left + right walls (4 → 6)");
}

/// Structure-phase WALL carve in SELECT mode whose ring runs entirely along
/// existing walls (it crosses none mid-span) must STILL select the enclosed
/// walls — carve is a selection gesture, so "no interior cut edge" means
/// "nothing to subdivide", not "do nothing". Regression: this used to bail with
/// only a `log::warn`, so lassoing an already-bounded region (e.g. a step leg)
/// silently selected nothing. A loop enclosing the whole 10×10 storey yields a
/// region coincident with the outline → zero interior cut edges.
#[test]
fn structure_wall_carve_on_existing_boundary_selects_without_splitting() {
    let mut c = dimensioned_square_controller();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Walls);
    c.set_structure_select_mode(Some(SelectMode::Carve));

    let walls_before = c.current_surface_counts().walls;

    // A loop enclosing the entire storey → the carved area IS the outline, so
    // every edge lies on an existing wall (no interior cut). The outline walls
    // are already dimensioned, so the split commits immediately.
    c.apply_structure_carve_stroke(&[
        (-1.0, -1.0),
        (11.0, -1.0),
        (11.0, 11.0),
        (-1.0, 11.0),
        (-0.9, -0.9),
    ]);
    // It may arrive as a pick-one cut or an immediate area; if a pick is
    // pending, choosing a point inside the storey commits it.
    if c.structure_cut_pending() {
        c.choose_structure_cut_region(5.0, 5.0);
    }
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        c.skip_current_dimension();
        guard += 1;
    }
    skip_position_prompts(&mut c);

    // Carve is now deferred: the ring is staged, and "Edit attributes" commits
    // it — which, finding no wall to subdivide, just selects the enclosed walls.
    c.begin_structure_edit();

    assert!(
        !c.structure_selected_ids().is_empty(),
        "the enclosed walls are selected even though nothing was subdivided: {:?}",
        c.structure_selected_ids()
    );
    assert!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .wall_splits
            .is_empty(),
        "a carve that crosses no wall stores NO durable wall split"
    );
    c.materialize();
    assert_eq!(
        c.current_surface_counts().walls,
        walls_before,
        "no wall was subdivided (count unchanged)"
    );
    assert_eq!(
        c.document.active,
        ActiveShape::Outline,
        "back on the outline"
    );
}

/// A STRUCTURE carve levels onto construction-phase lines exactly like a
/// living-area carve: the wall drawn ~0.3 m past the x=5 phase division lands
/// ON it, its parameter is pre-known (pinned — the pin matches the snap, so
/// dimensioning never asks for it), and the leveling surfaces as a skippable
/// snap tag ("Don't snap").
#[test]
fn structure_carve_snaps_to_phase_line_and_pins_the_wall() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10 m
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
        phase: ConstructionPhase::Extension1,
    });
    c.mark_dirty().unwrap();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Floors);
    c.set_structure_select_mode(Some(SelectMode::Carve));

    // Interior carve whose right wall is drawn 0.3 m past the phase division.
    c.apply_structure_carve_stroke(&[(2.0, 2.0), (5.3, 2.0), (5.3, 8.0), (2.0, 8.0), (2.1, 2.1)]);
    assert!(c.dimensioning_active(), "the carve starts dimensioning");
    let pts = c.document.floor_plan.get_points();
    let max_x = pts.iter().map(|v| v.x).fold(f32::MIN, f32::max);
    assert!(
        (max_x - 5.0).abs() < 1e-2,
        "the carved ring's wall snapped onto the phase line x=5: {pts:?}"
    );

    // The pin matches the snap: the snapped wall's coordinate parameter is
    // pre-known, so no dimension prompt opens for it.
    let (ring, edge) = c
        .document
        .floor_plan
        .hit_test_wall(5.0, 5.0, 0.05)
        .expect("the snapped wall sits at x=5");
    let (param, _axis) = c
        .document
        .floor_plan
        .wall_movable_param(ring, edge)
        .expect("the snapped wall has a coordinate parameter");
    assert!(
        c.document.dimensioning.known_params.contains(&param),
        "the phase-snapped wall is pinned pre-known: known {:?}",
        c.document.dimensioning.known_params
    );

    // The leveling is elective and visible: a skippable "Don't snap" tag.
    let tags = c.snap_overlay();
    assert!(
        tags.iter()
            .any(|t| t.skippable && t.current_kind == Some(manip_plan::SnapKind::ToParallelWall)),
        "the phase-line leveling surfaces as a skippable snap tag: {tags:?}"
    );
}

/// Wall inclusion is explicit where AMBIGUOUS: a carve edge on the storey
/// outline (true exterior — nothing beyond it) auto-includes its wall, while
/// an interior cut edge that a wall region actually STANDS ON (here a
/// floor-step RISER along x=5) raises the per-edge ask — exactly ONE ask, for
/// exactly that edge (interior edges over open floor never ask; see
/// `interior_structure_carve_through_open_floor_asks_nothing`). Include keeps
/// the riser; Exclude leaves it out. The two runs differ only in the answer.
#[test]
fn structure_carve_asks_for_interior_edge_walls_and_honours_the_answer() {
    let run = |include: bool| -> (bool, bool) {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = dimensioned_square_controller();
        // A floor step over the right half derives a RISER wall region along
        // the step line x=5 — the interior wall the ask decides about.
        c.step_regions.entry(0).or_default().push(StepRegion {
            polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
            level_m: -0.3,
        });
        c.commit_to_ber().unwrap();

        set_only_kind(&mut c, SurfaceKind::Walls);
        c.set_structure_select_mode(Some(SelectMode::Carve));

        // Straight cut along the step line, pick the RIGHT half: its x=10 edge
        // lies on the outline (exterior), its x=5 edge is interior.
        c.apply_structure_carve_stroke(&[(5.0, 0.0), (5.0, 10.0)]);
        assert!(c.structure_cut_pending(), "the cut offers two candidates");
        c.choose_structure_cut_region(7.5, 5.0);
        let mut guard = 0;
        while c.dimensioning_active() && guard < 12 {
            c.skip_current_dimension();
            guard += 1;
        }
        skip_position_prompts(&mut c);

        // Only the interior edge asks; the outline edges are auto-included.
        let prompt = c
            .wall_inclusion_prompt()
            .expect("the interior cut edge raises the ask");
        assert_eq!(prompt.total, 1, "exactly one ambiguous edge (x=5)");
        assert!(
            (prompt.edge.0 .0 - 5.0).abs() < 1e-3 && (prompt.edge.1 .0 - 5.0).abs() < 1e-3,
            "the asked edge runs along x=5: {:?}",
            prompt.edge
        );
        // The question names its target: the x=5 edge is the LEFT side of the
        // carved right half, runs the full 10 m, and carries the riser.
        assert_eq!(
            prompt.description, "Include the 10.0 m wall on the left edge (floor-step riser)?",
            "the description identifies side, length and wall kind"
        );
        assert_eq!(
            c.pending_structure_carve_polygons().len(),
            1,
            "the ring stays shaded while the ask is up"
        );
        c.resolve_wall_inclusion(include);
        assert!(c.wall_inclusion_prompt().is_none(), "the ask is answered");

        c.begin_structure_edit();
        let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
        // Identify walls by their base-edge MIDPOINT (a step z-jump can leave a
        // perimeter piece with two leading vertices at the same plan point, so
        // "first two vertices" is not a reliable base signature).
        let base_at = |r: &&ber_model::ber::structural::WallRegion, x: f64, y: f64| {
            r.base_midpoint()
                .is_some_and(|m| (m.x - x).abs() < 1e-3 && (m.y - y).abs() < 1e-3)
        };
        let riser = storey
            .walls
            .regions
            .iter()
            .find(|r| base_at(r, 5.0, 5.0))
            .expect("the floor-step riser stands on x=5");
        let right = storey
            .walls
            .regions
            .iter()
            .find(|r| base_at(r, 10.0, 5.0))
            .expect("the outline's right wall");
        (
            c.structure_selected_ids().contains(&riser.id),
            c.structure_selected_ids().contains(&right.id),
        )
    };

    let (riser_in, right_in) = run(true);
    assert!(right_in, "the exterior wall (on the outline) auto-included");
    assert!(riser_in, "Include keeps the wall along the interior edge");

    let (riser_in, right_in) = run(false);
    assert!(
        right_in,
        "the exterior wall is included regardless of the answer"
    );
    assert!(!riser_in, "Exclude leaves the interior-edge wall out");
}

/// A Walls-mode carve with NO outline contact through OPEN FLOOR asks nothing:
/// its edges are all interior, but no wall region stands on any of them, so
/// there is no wall whose inclusion could be decided — the carve stages
/// immediately, with no walls auto-included.
#[test]
fn interior_structure_carve_through_open_floor_asks_nothing() {
    let mut c = dimensioned_square_controller();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Walls);
    c.set_structure_select_mode(Some(SelectMode::Carve));

    // Interior 4×4 at (2,2)–(6,6): no edge touches the outline, and the plain
    // 10×10 storey has no interior wall regions (no risers) — open floor.
    c.apply_structure_carve_stroke(&[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)]);
    complete_interior_length_loop(&mut c);
    skip_position_prompts(&mut c);

    assert!(
        c.wall_inclusion_prompt().is_none(),
        "no wall stands on any edge — nothing to ask: {:?}",
        c.wall_inclusion_prompt()
    );
    assert_eq!(
        c.pending_structure_carve_polygons().len(),
        1,
        "the carve staged immediately, no ask in the way"
    );

    c.begin_structure_edit();
    assert!(
        c.structure_selected_ids().is_empty(),
        "no wall was auto-included from a fully interior carve: {:?}",
        c.structure_selected_ids()
    );
}

/// With SEVERAL ambiguous edges, the ask walks them one at a time and the
/// question's identification channels stay in lock-step: the highlighted edge
/// (`prompt.edge` — the canvas badge sits at its midpoint) and the composed
/// description always refer to the SAME edge, and both advance together with
/// the cursor. Fixture: two floor steps (left and right columns) derive risers
/// along x=2 and x=8; an interior carve spanning between them has walls on
/// exactly those two edges — one "left", one "right".
#[test]
fn wall_inclusion_ask_advances_description_and_marker_together() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    for polygon in [
        vec![p(0., 0.), p(2., 0.), p(2., 10.), p(0., 10.)], // left column
        vec![p(8., 0.), p(10., 0.), p(10., 10.), p(8., 10.)], // right column
    ] {
        c.step_regions.entry(0).or_default().push(StepRegion {
            polygon,
            level_m: -0.3,
        });
    }
    c.commit_to_ber().unwrap();

    set_only_kind(&mut c, SurfaceKind::Walls);
    c.set_structure_select_mode(Some(SelectMode::Carve));

    // Interior carve between the risers: left edge on x=2, right edge on x=8,
    // top/bottom edges over open floor (filtered out — no wall stands there).
    // The lift point stays within the 5 cm wall-on-edge tolerance of x=2: it
    // becomes the carve's anchored constant, i.e. the LEFT edge's coordinate.
    c.apply_structure_carve_stroke(&[(2.0, 2.0), (8.0, 2.0), (8.0, 8.0), (2.0, 8.0), (2.02, 2.02)]);
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        c.skip_current_dimension();
        guard += 1;
    }
    skip_position_prompts(&mut c);

    // Which side an edge sits on, from its geometry — what the badge shows.
    let side_of = |edge: ((f64, f64), (f64, f64))| {
        if (edge.0 .0 + edge.1 .0) * 0.5 < 5.0 {
            "left"
        } else {
            "right"
        }
    };
    // The description restates the edge's own geometry (self-consistency: the
    // words match the highlight, whatever the exact staged coordinates).
    let expect_for = |prompt: &WallInclusionPromptView| {
        let (a, b) = prompt.edge;
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        format!(
            "Include the {len:.1} m wall on the {} edge (floor-step riser)?",
            side_of(prompt.edge)
        )
    };

    let p0 = c.wall_inclusion_prompt().expect("two riser edges ask");
    assert_eq!(p0.total, 2, "both riser edges are ambiguous, nothing else");
    assert_eq!(p0.index, 0);
    assert_eq!(p0.description, expect_for(&p0), "words match the highlight");

    c.resolve_wall_inclusion(true);
    let p1 = c.wall_inclusion_prompt().expect("the ask advances");
    assert_eq!(p1.index, 1, "cursor advanced");
    assert_ne!(p1.edge, p0.edge, "the highlight moved to the OTHER edge");
    assert_eq!(
        p1.description,
        expect_for(&p1),
        "words follow the highlight"
    );
    assert_ne!(
        side_of(p0.edge),
        side_of(p1.edge),
        "one left edge and one right edge were asked"
    );

    c.resolve_wall_inclusion(true);
    assert!(c.wall_inclusion_prompt().is_none(), "both edges answered");
    assert_eq!(
        c.pending_structure_carve_polygons().len(),
        1,
        "the carve staged after the last answer"
    );
}

// ----- Inter-storey derivation through the real pipeline -----------------

/// Drop a dimensioned square outline of `side` offset to `(ox, oy)` into the
/// current storey — for building a SETBACK stack where the upper storey only
/// partly overlaps the lower.
fn finalise_current_storey_as_offset_square(c: &mut AppController, side: f32, ox: f32, oy: f32) {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    c.document.floor_plan = FloorPlan::from_points_to_orthogonal(
        &[
            p(ox, oy),
            p(ox + side, oy),
            p(ox + side, oy + side),
            p(ox, oy + side),
            p(ox + 0.05, oy + 0.05),
        ],
        0.1,
        true,
    )
    .unwrap();
    c.document.dimensioning.scale_set = true;
    c.document.dimensioning.known_params = c.document.floor_plan.all_active_params();
    c.commit_to_ber().unwrap();
}

/// On a 2-storey SETBACK stack (lower 10×10 at origin, upper 6×6 offset to
/// (6,6) so it overhangs the lower on the top-right), the pipeline's
/// `derive_inter_storey` tail — no tool pass, just `materialize` — splits the
/// partial overlaps and stamps the junction sub-regions:
///  - the upper storey's FLOOR splits, its piece over the lower → InterStorey;
///  - the lower storey's CEILING splits, its piece under the upper → InterStorey.
/// The non-overlapping pieces stay Unset (left for manual marking).
#[test]
fn setback_stack_derives_inter_storey_floor_and_ceiling_pieces() {
    use ber_model::ber::structural::{CeilingExposure, FloorExposure};

    let mut c = dimensioned_square_controller(); // storey 0 = 10×10 at origin
                                                 // Give the lower storey a ceiling so it has roof regions to split.
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    finalise_current_storey_as_offset_square(&mut c, 6.0, 6.0, 6.0); // upper 6×6 @ (6,6)
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();

    let storeys = &c.current_record.as_ref().unwrap().data.storeys;

    // Upper storey's FLOOR split into the over-lower piece (InterStorey) + the
    // overhang piece (Unset). The overlap is x∈[6,10], y∈[6,10].
    assert_eq!(
        storeys[1].floors.regions.len(),
        2,
        "upper floor split in two by the below footprint"
    );
    // Identify the pieces by GEOMETRY, not push order: the junction piece is
    // the 4×4 overlap square [6,10]² (area 16, centroid (8,8)); the overhang
    // is the L (area 20, centroid ≈(9.8, 9.8) — note its centroid ALSO sits
    // inside [6,10]², so a bare "x<10 && y<10" match would be order-dependent).
    let junction_floor = storeys[1]
        .floors
        .regions
        .iter()
        .find(|r| (r.area() - 16.0).abs() < 1e-3)
        .expect("the 16 m² over-lower floor piece");
    let c = junction_floor.centroid();
    assert!(
        (c.x - 8.0).abs() < 1e-3 && (c.y - 8.0).abs() < 1e-3,
        "junction piece is the [6,10]² overlap square, centroid {c:?}"
    );
    assert_eq!(
        junction_floor.exposure,
        FloorExposure::InterStorey,
        "the piece over the lower storey is a derived storey junction"
    );
    let overhang = storeys[1]
        .floors
        .regions
        .iter()
        .find(|r| (r.area() - 20.0).abs() < 1e-3)
        .expect("the 20 m² overhang L-piece");
    assert_eq!(
        overhang.exposure,
        FloorExposure::Unset,
        "the overhang piece is left Unset"
    );

    // Lower storey's CEILING split, the piece under the upper → InterStorey.
    assert!(
        storeys[0].ceilings.regions.len() >= 2,
        "lower ceiling split by the upper footprint"
    );
    assert!(
        storeys[0]
            .ceilings
            .regions
            .iter()
            .any(|r| r.exposure == CeilingExposure::InterStorey),
        "the piece under the upper storey is a derived storey junction"
    );
    // Nothing was pushed onto the durable split lists — the floor split is
    // derived-only and the ceiling split rides regions-as-truth.
    assert!(
        storeys.iter().all(|s| s.floor_splits.is_empty()),
        "no durable floor_splits from the derivation"
    );
    assert!(
        storeys.iter().all(|s| s.ceiling_splits.is_empty()),
        "no durable ceiling_splits from the derivation"
    );
}

/// Floor-exposure tool — Set-all then assign: entering raises the gate, confirm
/// reveals the view, `Set all` seeds every floor region
/// across the stack, and `set_floor_exposure_for(Ground)` stamps each one.
#[test]
fn floor_exposure_set_all_then_assign_grounds_every_floor() {
    use ber_model::ber::structural::FloorExposure;
    let mut c = dimensioned_square_controller();
    c.materialize();

    c.set_floor_exposure_mode(true);
    assert!(c.floor_exposure_mode());
    assert!(
        c.floor_exposure_gate_pending(),
        "entering the tool raises the storey-count gate"
    );

    c.confirm_all_storeys_added();
    assert!(
        !c.floor_exposure_gate_pending(),
        "confirming lowers the gate"
    );

    c.set_floor_exposure_select_mode(Some(SelectMode::All));
    let n_regions: usize = c.current_record.as_ref().unwrap().data.storeys[0]
        .floors
        .regions
        .len();
    assert_eq!(
        c.floor_exposure_selected_ids().len(),
        n_regions,
        "Set all seeds every floor region across the stack"
    );

    c.set_floor_exposure_for(FloorExposure::Ground);
    c.materialize();

    let rec = c.current_record.as_ref().unwrap();
    assert!(
        rec.data.storeys[0]
            .floors
            .regions
            .iter()
            .all(|r| r.exposure == FloorExposure::Ground),
        "every floor region resolves to the assigned Ground exposure"
    );
}

/// Floor-exposure CLICK toggles a single region in and back out of the
/// selection (the smallest-area overlap-pick rule degenerates to the one region
/// here), and the gate blocks selection until confirmed.
#[test]
fn floor_exposure_click_toggles_one_region() {
    let mut c = dimensioned_square_controller();
    c.materialize();
    // A point inside the square footprint (dimensioned_square is ~ a unit-ish
    // square near the origin; its centroid is well inside).
    let region = &c.current_record.as_ref().unwrap().data.storeys[0]
        .floors
        .regions[0];
    let centre = region.centroid();

    c.set_floor_exposure_mode(true);
    c.set_floor_exposure_select_mode(Some(SelectMode::Click));
    // Gate still up → select mode is rejected, taps do nothing.
    c.toggle_floor_exposure_at(centre.x, centre.y, 0.5);
    assert!(
        c.floor_exposure_selected_ids().is_empty(),
        "the gate blocks selection until storeys are confirmed"
    );

    c.confirm_all_storeys_added();
    c.set_floor_exposure_select_mode(Some(SelectMode::Click));
    c.toggle_floor_exposure_at(centre.x, centre.y, 0.5);
    assert_eq!(
        c.floor_exposure_selected_ids().len(),
        1,
        "a tap selects the floor region under it"
    );
    c.toggle_floor_exposure_at(centre.x, centre.y, 0.5);
    assert!(
        c.floor_exposure_selected_ids().is_empty(),
        "a second tap toggles it back out"
    );
}

/// Floor-exposure CARVE: drawing + dimensioning a carve SPLITS the floor (it
/// doesn't just select whole regions, the old behaviour) and selects the carved
/// piece so an exposure can be assigned to it — the phase-2 carve flow. Only the
/// LABELLABLE (non-InterStorey) floors are split/selectable: in a same-footprint
/// stack the upper floor derives InterStorey (a storey junction) and is left
/// untouched, while the lower (Unset) floor carves and selects.
#[test]
fn floor_exposure_carve_dimensions_then_splits_the_labellable_floor() {
    use ber_model::ber::structural::FloorExposure;

    // Two storeys, SAME 10×10 footprint at the origin. The pipeline's
    // InterStorey derivation classifies the UPPER floor (fully over the lower)
    // and leaves the LOWER floor Unset (the lowest floor is never
    // auto-classified).
    let mut c = dimensioned_square_controller(); // storey 0 = 10×10 at origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    finalise_current_storey_as_offset_square(&mut c, 10.0, 0.0, 0.0); // storey 1 = 10×10 @ origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();
    {
        let s = &c.current_record.as_ref().unwrap().data.storeys;
        assert_eq!(s[0].floors.regions.len(), 1);
        assert_eq!(s[1].floors.regions.len(), 1);
    }

    c.set_floor_exposure_mode(true);
    c.confirm_all_storeys_added();
    c.materialize();
    // Derivation: upper floor InterStorey (hidden), lower floor still Unset
    // (labellable).
    {
        let s = &c.current_record.as_ref().unwrap().data.storeys;
        assert_eq!(s[0].floors.regions[0].exposure, FloorExposure::Unset);
        assert_eq!(s[1].floors.regions[0].exposure, FloorExposure::InterStorey);
    }
    c.set_floor_exposure_select_mode(Some(SelectMode::Carve));

    // An interior 4×4 carve at (2,2)-(6,6) — overlaps both footprints, but only
    // the labellable lower floor is split.
    c.apply_floor_exposure_carve_stroke(&[
        (2.0, 2.0),
        (6.0, 2.0),
        (6.0, 6.0),
        (2.0, 6.0),
        (2.1, 2.1),
    ]);
    assert!(
        c.dimensioning_active(),
        "the carve prompts to be dimensioned before it splits"
    );
    complete_interior_length_loop(&mut c);
    skip_position_prompts(&mut c);
    c.materialize();

    // The labellable LOWER floor split in two; the InterStorey UPPER floor is
    // untouched.
    {
        let s = &c.current_record.as_ref().unwrap().data.storeys;
        assert_eq!(
            s[0].floors.regions.len(),
            2,
            "lower (labellable) floor carved in two"
        );
        assert_eq!(
            s[1].floors.regions.len(),
            1,
            "InterStorey upper floor left untouched"
        );
    }
    assert!(
        !c.floor_exposure_selected_ids().is_empty(),
        "the carved lower piece is selected for exposure-setting"
    );

    // Assigning an exposure marks ONLY the carved lower piece — NOT the frame
    // remainder around it. (This is an interior carve, so the remainder is a
    // concave frame whose area-weighted centroid falls in the carved hole; the
    // old centroid-based select grabbed it too and set the whole floor. The
    // area-fraction select picks only the piece actually inside the ring.)
    c.set_floor_exposure_for(FloorExposure::Exposed);
    c.materialize();
    let s = &c.current_record.as_ref().unwrap().data.storeys;
    let exposed = s[0]
        .floors
        .regions
        .iter()
        .filter(|r| r.exposure == FloorExposure::Exposed)
        .count();
    assert_eq!(
        exposed, 1,
        "exactly the one carved piece is set — not the surrounding remainder"
    );
    assert_eq!(
        s[1].floors.regions[0].exposure,
        FloorExposure::InterStorey,
        "the InterStorey upper floor is unaffected"
    );
}

/// Regression: the floor-exposure carve clips to the WHOLE BUILDING footprint
/// (union of all storeys), not the reference storey's outline. Opening the tool
/// from the smaller UPPER storey (so it's the reference) and carving over area
/// that exists only on the LOWER storey must still produce a valid carve + split
/// the lower floor — the old behaviour clipped the stroke to the upper outline
/// and lost it.
#[test]
fn floor_exposure_carve_clips_to_whole_building_not_reference_storey() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10×10 at origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    finalise_current_storey_as_offset_square(&mut c, 6.0, 0.0, 0.0); // storey 1 = 6×6 @ origin (setback)
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();
    // The upper (smaller) storey is active — as if the assessor clicked it first.
    assert_eq!(c.current_storey_idx(), Some(1));

    c.set_floor_exposure_mode(true);
    c.confirm_all_storeys_added();
    c.set_floor_exposure_select_mode(Some(SelectMode::Carve));

    // Carve a region near the (10,10) corner — inside the LOWER 10×10 floor but
    // entirely OUTSIDE the upper 6×6 (the reference). The whole-building union is
    // the 10×10, so this is a valid carve; clipped to the upper outline it would
    // be empty.
    c.apply_floor_exposure_carve_stroke(&[
        (7.0, 7.0),
        (9.0, 7.0),
        (9.0, 9.0),
        (7.0, 9.0),
        (7.1, 7.1),
    ]);
    // The carve produced a valid dimensionable area: it was clipped to the whole
    // building (the 10×10 union), not the smaller upper storey. Under the old
    // behaviour the stroke lay entirely outside the reference (upper) outline, so
    // no area was carved and dimensioning never started. (The split + selection
    // after dimensioning is covered by the sibling carve test.)
    assert!(
        c.dimensioning_active(),
        "the carve survives — clipped to the whole building, not the smaller upper storey"
    );
    assert!(
        c.pending_floor_exposure_carve_polygon().len() >= 3,
        "the in-flight carve region is live"
    );
    // The "stay inside the storey" dimension guardrail checks the WHOLE-building
    // containment override (the 10-wide union), not the smaller upper (6-wide)
    // outline — so dimensioning the carve out toward the lower-storey extent is
    // accepted. (This is what rejected a valid length before the fix.)
    let ov = c.document.storeys[1]
        .subarea_containment_override
        .as_ref()
        .expect("the carve sets a whole-building containment override");
    let ((min_x, _), (max_x, _)) = ov.bounding_box().expect("override has a bbox");
    assert!(
        max_x - min_x > 8.0,
        "containment spans the whole building (~10 wide), not the upper storey (~6)"
    );
}

/// Floor-exposure straight CUT: an edge-to-edge stroke splits the whole building
/// into two candidate halves and WAITS for a side pick (nothing split yet);
/// tapping one half begins dimensioning it; once dimensioned the chosen side
/// splits the labellable (non-InterStorey) floor and is selected — mirrors the
/// structure-phase cut + the floor-exposure Area carve.
#[test]
fn floor_exposure_cut_picks_side_dimensions_then_splits_the_labellable_floor() {
    use ber_model::ber::structural::FloorExposure;

    // Two storeys, SAME 10×10 footprint. The InterStorey derivation classifies
    // the UPPER floor (hidden) and leaves the LOWER floor Unset (labellable).
    let mut c = dimensioned_square_controller(); // storey 0 = 10×10 at origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    finalise_current_storey_as_offset_square(&mut c, 10.0, 0.0, 0.0); // storey 1 = 10×10 @ origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();

    c.set_floor_exposure_mode(true);
    c.confirm_all_storeys_added();
    c.materialize();
    {
        let s = &c.current_record.as_ref().unwrap().data.storeys;
        assert_eq!(s[0].floors.regions[0].exposure, FloorExposure::Unset);
        assert_eq!(s[1].floors.regions[0].exposure, FloorExposure::InterStorey);
        assert_eq!(s[0].floors.regions.len(), 1, "lower floor whole");
    }
    c.set_floor_exposure_select_mode(Some(SelectMode::Carve));

    // A straight edge-to-edge cut at y=5 (left wall to right wall) splits the
    // whole building into two halves — it WAITS for the side pick.
    c.apply_floor_exposure_carve_stroke(&[(0.0, 5.0), (10.0, 5.0)]);
    assert!(
        c.floor_exposure_cut_pending(),
        "a straight cut offers two candidate halves to pick"
    );
    assert_eq!(c.floor_exposure_cut_regions().len(), 2, "two candidates");
    assert!(
        !c.dimensioning_active(),
        "the cut does NOT begin dimensioning until a side is picked"
    );
    // Nothing split yet — the lower floor is still whole.
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .floors
            .regions
            .len(),
        1,
        "no split until a side is chosen"
    );

    // Pick the lower half (y<5) → it becomes the in-flight carve shape and starts
    // dimensioning.
    c.choose_floor_exposure_cut_region(5.0, 2.0);
    assert!(
        !c.floor_exposure_cut_pending(),
        "pick consumed the candidates"
    );
    assert!(
        c.dimensioning_active() || c.document.dimensioning.current_position_prompt.is_some(),
        "the picked half prompts to be dimensioned"
    );

    // Accept the drawn cut (skip lengths so it stays at y=5) + accept positions;
    // the chosen side stages + commits on the last skip.
    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        c.skip_current_dimension();
        guard += 1;
    }
    skip_position_prompts(&mut c);
    c.materialize();

    // The labellable LOWER floor split in two; the InterStorey UPPER floor
    // untouched.
    {
        let s = &c.current_record.as_ref().unwrap().data.storeys;
        assert_eq!(
            s[0].floors.regions.len(),
            2,
            "lower (labellable) floor cut in two"
        );
        assert_eq!(
            s[1].floors.regions.len(),
            1,
            "InterStorey upper floor untouched"
        );
    }
    assert!(
        !c.floor_exposure_selected_ids().is_empty(),
        "the chosen side is selected for exposure-setting"
    );
}

// ── Ceiling-exposure tool (mirror of the floor-exposure tool tests) ──────────

/// Ceiling-exposure tool — Set-all then assign: entering raises the gate,
/// confirm reveals the view, `Set all` seeds every
/// (non-party) ceiling region across the stack, and `set_ceiling_exposure_for`
/// stamps each one. Single storey, so its top ceiling is Unset (labellable).
#[test]
fn ceiling_exposure_set_all_then_assign_marks_every_ceiling() {
    use ber_model::ber::structural::CeilingExposure;
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();

    c.set_ceiling_exposure_mode(true);
    assert!(c.ceiling_exposure_mode());
    assert!(
        c.ceiling_exposure_gate_pending(),
        "entering the tool raises the storey-count gate"
    );

    c.confirm_all_storeys_added_ceiling();
    assert!(
        !c.ceiling_exposure_gate_pending(),
        "confirming lowers the gate"
    );

    c.set_ceiling_exposure_select_mode(Some(SelectMode::All));
    let n_regions: usize = c.current_record.as_ref().unwrap().data.storeys[0]
        .ceilings
        .regions
        .len();
    assert!(n_regions >= 1);
    assert_eq!(
        c.ceiling_exposure_selected_ids().len(),
        n_regions,
        "Set all seeds every ceiling region across the stack"
    );

    c.set_ceiling_exposure_for(CeilingExposure::ExposedRoof);
    c.materialize();
    let rec = c.current_record.as_ref().unwrap();
    assert!(
        rec.data.storeys[0]
            .ceilings
            .regions
            .iter()
            .all(|r| r.exposure == CeilingExposure::ExposedRoof),
        "every ceiling region resolves to the assigned ExposedRoof exposure"
    );
}

/// Ceiling-exposure CLICK toggles a single region in and back out of the
/// selection, and the gate blocks selection until confirmed.
#[test]
fn ceiling_exposure_click_toggles_one_region() {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();
    let region = &c.current_record.as_ref().unwrap().data.storeys[0]
        .ceilings
        .regions[0];
    let centre = region.centroid();

    c.set_ceiling_exposure_mode(true);
    c.set_ceiling_exposure_select_mode(Some(SelectMode::Click));
    // Gate still up → select mode is rejected, taps do nothing.
    c.toggle_ceiling_exposure_at(centre.x, centre.y, 0.5);
    assert!(
        c.ceiling_exposure_selected_ids().is_empty(),
        "the gate blocks selection until storeys are confirmed"
    );

    c.confirm_all_storeys_added_ceiling();
    c.set_ceiling_exposure_select_mode(Some(SelectMode::Click));
    c.toggle_ceiling_exposure_at(centre.x, centre.y, 0.5);
    assert_eq!(
        c.ceiling_exposure_selected_ids().len(),
        1,
        "a tap selects the ceiling region under it"
    );
    c.toggle_ceiling_exposure_at(centre.x, centre.y, 0.5);
    assert!(
        c.ceiling_exposure_selected_ids().is_empty(),
        "a second tap toggles it back out"
    );
}

/// The InterStorey derivation on a same-footprint 2-storey stack leaves the
/// TOP ceiling Unset (the assessor's call — exposed roof vs unheated attic)
/// and stamps only the intermediate ceiling under the storey above
/// `InterStorey` (a storey junction of this dwelling).
#[test]
fn derivation_leaves_top_ceiling_unset_and_stamps_intermediate_inter_storey() {
    use ber_model::ber::structural::CeilingExposure;
    let mut c = dimensioned_square_controller(); // storey 0 = 10×10 at origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    finalise_current_storey_as_offset_square(&mut c, 10.0, 0.0, 0.0); // storey 1 = 10×10 @ origin
    c.set_default_ceiling_height(2.4).unwrap();

    c.set_ceiling_exposure_mode(true);
    c.confirm_all_storeys_added_ceiling();
    c.materialize();

    let s = &c.current_record.as_ref().unwrap().data.storeys;
    assert_eq!(
        s[0].ceilings.regions[0].exposure,
        CeilingExposure::InterStorey,
        "the intermediate ceiling (under the storey above) derives InterStorey"
    );
    assert_eq!(
        s[1].ceilings.regions[0].exposure,
        CeilingExposure::Unset,
        "the top ceiling is left Unset for the assessor, not auto ExposedRoof"
    );
}

/// Ceiling-exposure CARVE: drawing + dimensioning a carve SPLITS the ROOFS
/// (regions-as-truth, via recompose) and selects the carved piece. Only the
/// LABELLABLE (non-InterStorey) ceiling is split: on a same-footprint stack the
/// LOWER ceiling derives InterStorey (hidden) and is left untouched, while the
/// TOP (Unset) ceiling carves and selects.
#[test]
fn ceiling_exposure_carve_dimensions_then_splits_the_labellable_ceiling() {
    use ber_model::ber::structural::CeilingExposure;
    let mut c = dimensioned_square_controller(); // storey 0 = 10×10 at origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    finalise_current_storey_as_offset_square(&mut c, 10.0, 0.0, 0.0); // storey 1 = 10×10 @ origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();
    {
        let s = &c.current_record.as_ref().unwrap().data.storeys;
        assert_eq!(s[0].ceilings.regions.len(), 1);
        assert_eq!(s[1].ceilings.regions.len(), 1);
    }

    c.set_ceiling_exposure_mode(true);
    c.confirm_all_storeys_added_ceiling();
    c.materialize();
    {
        let s = &c.current_record.as_ref().unwrap().data.storeys;
        assert_eq!(
            s[0].ceilings.regions[0].exposure,
            CeilingExposure::InterStorey
        );
        assert_eq!(s[1].ceilings.regions[0].exposure, CeilingExposure::Unset);
    }
    c.set_ceiling_exposure_select_mode(Some(SelectMode::Carve));

    // An interior 4×4 carve overlapping both footprints; only the labellable top
    // ceiling is split.
    c.apply_ceiling_exposure_carve_stroke(&[
        (2.0, 2.0),
        (6.0, 2.0),
        (6.0, 6.0),
        (2.0, 6.0),
        (2.1, 2.1),
    ]);
    assert!(
        c.dimensioning_active(),
        "the carve prompts to be dimensioned before it splits"
    );
    complete_interior_length_loop(&mut c);
    skip_position_prompts(&mut c);
    c.materialize();

    {
        let s = &c.current_record.as_ref().unwrap().data.storeys;
        // The roof carve splits the top ceiling into the inner piece + its
        // surrounding frame (`split_faces_coplanar` may tessellate the frame into
        // more than one piece, unlike the floor boolean — so assert "carved", > 1).
        assert!(
            s[1].ceilings.regions.len() > 1,
            "top (labellable) ceiling carved into the inner piece + frame"
        );
        assert_eq!(
            s[0].ceilings.regions.len(),
            1,
            "InterStorey lower ceiling left untouched"
        );
    }
    assert!(
        !c.ceiling_exposure_selected_ids().is_empty(),
        "the carved top piece is selected for exposure-setting"
    );

    c.set_ceiling_exposure_for(CeilingExposure::ExposedRoof);
    c.materialize();
    let s = &c.current_record.as_ref().unwrap().data.storeys;
    assert!(
        s[1].ceilings
            .regions
            .iter()
            .any(|r| r.exposure == CeilingExposure::ExposedRoof),
        "the carved top piece took the assigned exposure"
    );
    assert_eq!(
        s[0].ceilings.regions[0].exposure,
        CeilingExposure::InterStorey,
        "the InterStorey lower ceiling is unaffected"
    );
}

/// Regression mirror: the ceiling-exposure carve clips to the WHOLE BUILDING
/// footprint (union of all storeys), not the reference storey's outline. Opening
/// the tool from the smaller UPPER storey and carving over area that exists only
/// on the LOWER storey must still produce a valid carve.
#[test]
fn ceiling_exposure_carve_clips_to_whole_building_not_reference_storey() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10×10 at origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    finalise_current_storey_as_offset_square(&mut c, 6.0, 0.0, 0.0); // storey 1 = 6×6 @ origin (setback)
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();
    assert_eq!(c.current_storey_idx(), Some(1));

    c.set_ceiling_exposure_mode(true);
    c.confirm_all_storeys_added_ceiling();
    c.set_ceiling_exposure_select_mode(Some(SelectMode::Carve));

    // Carve near the (10,10) corner — inside the LOWER 10×10 but entirely OUTSIDE
    // the upper 6×6 (the reference). The whole-building union is the 10×10, so
    // this survives; clipped to the upper outline it would be empty.
    c.apply_ceiling_exposure_carve_stroke(&[
        (7.0, 7.0),
        (9.0, 7.0),
        (9.0, 9.0),
        (7.0, 9.0),
        (7.1, 7.1),
    ]);
    assert!(
        c.dimensioning_active(),
        "the carve survives — clipped to the whole building, not the smaller upper storey"
    );
    assert!(
        c.pending_ceiling_exposure_carve_polygon().len() >= 3,
        "the in-flight carve region is live"
    );
    let ov = c.document.storeys[1]
        .subarea_containment_override
        .as_ref()
        .expect("the carve sets a whole-building containment override");
    let ((min_x, _), (max_x, _)) = ov.bounding_box().expect("override has a bbox");
    assert!(
        max_x - min_x > 8.0,
        "containment spans the whole building (~10 wide), not the upper storey (~6)"
    );
}

/// Ceiling-exposure straight CUT (mirror of the floor cut): an edge-to-edge
/// stroke splits the whole building into two candidate halves and WAITS for a
/// side pick; tapping one half dimensions it; the chosen side then splits the
/// labellable (non-InterStorey) top ceiling and is selected.
#[test]
fn ceiling_exposure_cut_picks_side_dimensions_then_splits_the_labellable_ceiling() {
    use ber_model::ber::structural::CeilingExposure;

    let mut c = dimensioned_square_controller(); // storey 0 = 10×10 at origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    finalise_current_storey_as_offset_square(&mut c, 10.0, 0.0, 0.0); // storey 1 = 10×10 @ origin
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();

    c.set_ceiling_exposure_mode(true);
    c.confirm_all_storeys_added_ceiling();
    c.materialize();
    {
        let s = &c.current_record.as_ref().unwrap().data.storeys;
        assert_eq!(
            s[0].ceilings.regions[0].exposure,
            CeilingExposure::InterStorey
        );
        assert_eq!(s[1].ceilings.regions[0].exposure, CeilingExposure::Unset);
        assert_eq!(s[1].ceilings.regions.len(), 1, "top ceiling whole");
    }
    c.set_ceiling_exposure_select_mode(Some(SelectMode::Carve));

    // A straight edge-to-edge cut at y=5 splits the whole building into two
    // halves — it WAITS for the side pick (nothing split yet).
    c.apply_ceiling_exposure_carve_stroke(&[(0.0, 5.0), (10.0, 5.0)]);
    assert!(
        c.ceiling_exposure_cut_pending(),
        "a straight cut offers two candidate halves to pick"
    );
    assert_eq!(c.ceiling_exposure_cut_regions().len(), 2, "two candidates");
    assert!(
        !c.dimensioning_active(),
        "the cut does NOT begin dimensioning until a side is picked"
    );
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[1]
            .ceilings
            .regions
            .len(),
        1,
        "no split until a side is chosen"
    );

    // Pick the lower half (y<5) → it starts dimensioning.
    c.choose_ceiling_exposure_cut_region(5.0, 2.0);
    assert!(
        !c.ceiling_exposure_cut_pending(),
        "pick consumed the candidates"
    );
    assert!(
        c.dimensioning_active() || c.document.dimensioning.current_position_prompt.is_some(),
        "the picked half prompts to be dimensioned"
    );

    let mut guard = 0;
    while c.dimensioning_active() && guard < 12 {
        c.skip_current_dimension();
        guard += 1;
    }
    skip_position_prompts(&mut c);
    c.materialize();

    {
        let s = &c.current_record.as_ref().unwrap().data.storeys;
        assert!(
            s[1].ceilings.regions.len() > 1,
            "top (labellable) ceiling cut into pieces"
        );
        assert_eq!(
            s[0].ceilings.regions.len(),
            1,
            "InterStorey lower ceiling untouched"
        );
    }
    assert!(
        !c.ceiling_exposure_selected_ids().is_empty(),
        "the chosen side is selected for exposure-setting"
    );
}

// ── Structure-phase group EXPOSURE (the editable dropdown) ──────────────────

/// (a) Two walls sharing the SAME (default) construction but with DIFFERENT
/// exposure split into two separate groups — the grouping key now includes
/// exposure.
#[test]
fn structure_groups_split_by_exposure() {
    let mut c = dimensioned_square_controller();
    let walls = c.document.outline.floor_plan.perimeter_walls();
    assert!(walls.len() >= 4);
    let (vid0, a0, b0) = walls[0];
    let (vid1, a1, b1) = walls[1];
    let len0 = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();
    let len1 = ((b1.x - a1.x).powi(2) + (b1.y - a1.y).powi(2)).sqrt();
    // Paint each wall full-length with a DIFFERENT exposure; construction is
    // left at its (shared) default, so only exposure distinguishes them.
    c.document
        .set_wall_section(vid0, 0.0, len0, len0, WallExposure::Exposed);
    c.document
        .set_wall_section(vid1, 0.0, len1, len1, WallExposure::SemiExposed);
    c.mark_dirty().unwrap(); // direct field writes don't bump the revision
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Walls);
    c.set_structure_select_mode(Some(SelectMode::All));
    c.begin_structure_edit();
    let e = c.structure_edit().expect("session opened");
    // The two painted walls land in distinct groups by their exposure; the two
    // unmarked walls form their own Unset group(s).
    use ber_model::ber::structural::SurfaceExposure;
    let exposed = e
        .groups
        .iter()
        .filter(|g| g.exposure == SurfaceExposure::Wall(WallExposure::Exposed))
        .count();
    let semi = e
        .groups
        .iter()
        .filter(|g| g.exposure == SurfaceExposure::Wall(WallExposure::SemiExposed))
        .count();
    assert_eq!(exposed, 1, "one Exposed group");
    assert_eq!(semi, 1, "one Semi-exposed group");
    assert!(
        e.groups.len() >= 3,
        "Exposed, Semi-exposed and at least one Unset group: {}",
        e.groups.len()
    );
}

/// (b) Setting a FLOOR group's exposure stamps every floor region's exposure
/// after `materialize`.
#[test]
fn structure_group_exposure_sets_floor_regions() {
    use ber_model::ber::structural::{FloorExposure, SurfaceExposure};
    let mut c = dimensioned_square_controller();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Floors);
    c.set_structure_select_mode(Some(SelectMode::All));
    c.begin_structure_edit();
    assert_eq!(
        c.structure_edit().unwrap().groups.len(),
        1,
        "one floor group"
    );

    c.set_structure_group_exposure(0, SurfaceExposure::Floor(FloorExposure::Ground));
    // Reflected immediately in the session group.
    assert_eq!(
        c.structure_edit().unwrap().groups[0].exposure,
        SurfaceExposure::Floor(FloorExposure::Ground),
    );
    c.materialize();

    let rec = c.current_record.as_ref().unwrap();
    assert!(
        rec.data.storeys[0]
            .floors
            .regions
            .iter()
            .all(|r| r.exposure == FloorExposure::Ground),
        "every floor region stamped Ground"
    );
}

/// (c) Setting a PERIMETER wall group's exposure repaints ONLY the targeted
/// regions' spans — a second, differently-exposed section on the SAME wall
/// keeps its exposure.
#[test]
fn structure_group_exposure_repaints_only_targeted_wall_span() {
    use ber_model::ber::structural::SurfaceExposure;
    let mut c = dimensioned_square_controller();
    let (vid0, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();
    // Two sections on wall0: [0, len/2] SemiExposed and [len/2, len] Exposed.
    c.document
        .set_wall_section(vid0, 0.0, len * 0.5, len, WallExposure::SemiExposed);
    c.document
        .set_wall_section(vid0, len * 0.5, len, len, WallExposure::Exposed);
    c.mark_dirty().unwrap(); // direct field writes don't bump the revision
    c.materialize();

    // Select ONLY the SemiExposed region of wall0 and re-expose it Ground.
    let semi_id = {
        let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
        storey
            .walls
            .regions
            .iter()
            .find(|r| r.exposure == WallExposure::SemiExposed)
            .expect("the semi-exposed half")
            .id
    };
    set_only_kind(&mut c, SurfaceKind::Walls);
    c.set_structure_select_mode(Some(SelectMode::Click));
    // Begin an edit over a single-id selection by toggling it in directly.
    c.toggle_structure_surface_at(a0.x as f64, a0.y as f64, 0.5); // ensure a click mode is live
    c.clear_structure_surface_selection();
    // Seed the selection with just the SemiExposed region's id.
    c.structure_selected.insert(semi_id);
    c.begin_structure_edit();
    let gi = c
        .structure_edit()
        .unwrap()
        .groups
        .iter()
        .position(|g| g.exposure == SurfaceExposure::Wall(WallExposure::SemiExposed))
        .expect("the semi-exposed group");
    c.set_structure_group_exposure(gi, SurfaceExposure::Wall(WallExposure::Ground));
    c.materialize();

    // The other half (Exposed) is untouched; the targeted half is now Ground.
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let on_wall0: Vec<WallExposure> = storey
        .walls
        .regions
        .iter()
        .filter_map(|r| {
            c.document.storeys[0]
                .recover_wall_region_span(r)
                .filter(|(vid, ..)| *vid == vid0)
                .map(|_| r.exposure)
        })
        .collect();
    assert!(
        on_wall0.contains(&WallExposure::Exposed),
        "the other section keeps Exposed: {on_wall0:?}"
    );
    assert!(
        !on_wall0.contains(&WallExposure::SemiExposed),
        "the targeted span is no longer SemiExposed: {on_wall0:?}"
    );
}

/// (d) A FACE-PARTITIONED wall's group is `exposure_locked` — the dropdown is
/// shown disabled (the per-region partition tool owns its exposure).
#[test]
fn face_partitioned_wall_group_is_exposure_locked() {
    use document::PartitionCut;
    let mut c = dimensioned_square_controller();
    let (vid0, ..) = c.document.outline.floor_plan.perimeter_walls()[0];
    // Face-partition wall0 with a single horizontal cut.
    c.document.commit_partition_cut(
        vid0,
        PartitionCut {
            a: Point2f32::new(0.0, 1.2),
            b: Point2f32::new(10.0, 1.2),
        },
    );
    assert!(c.document.is_wall_partitioned(vid0));
    c.mark_dirty().unwrap();
    c.materialize();

    set_only_kind(&mut c, SurfaceKind::Walls);
    c.set_structure_select_mode(Some(SelectMode::All));
    c.begin_structure_edit();
    let e = c.structure_edit().expect("session opened");
    assert!(
        e.groups.iter().any(|g| g.exposure_locked),
        "the partitioned wall's group is exposure-locked"
    );
}

// ─── Ceiling-exposure carve pre-snap to interior reference edges ─────────────

/// The ceiling-exposure carve gathers the interior roof-region division edges
/// as snap references. A ceiling step at x=4 splits the 10×10 roof into two
/// regions whose shared boundary is the vertical line x=4 ("end of the roof" /
/// flat-vs-sloped division). `ceiling_carve_reference_segments` must surface
/// that division so a carve traced near it can snap onto it — even though it's
/// NOT on the whole-building clip outline.
#[test]
fn ceiling_carve_reference_segments_include_the_roof_division() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).expect("amount applies");
    c.materialize();
    // Two roof regions now, divided at x=4.
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .ceilings
            .regions
            .len(),
        2,
        "ceiling step split the roof in two"
    );

    let segs = c.ceiling_carve_reference_segments();
    // A vertical reference segment along x=4 (the division) must be present.
    let has_division = segs.iter().any(|[a, b]| {
        (a.x - 4.0).abs() < 1e-3 && (b.x - 4.0).abs() < 1e-3 && (a.y - b.y).abs() > 1.0
    });
    assert!(
        has_division,
        "the flat/sloped roof division at x=4 is a snap reference: {segs:?}"
    );
}

/// A carve stroke whose edge runs NEAR (slightly off) the interior roof
/// division pre-snaps ONTO the division before `carve_or_cut` — so the carved
/// ceiling piece's boundary lands on the division, not the drawn offset. This
/// asserts the pre-snap step (the part `carve_or_cut`'s outline alignment can't
/// do, because the division isn't on the clip outline).
#[test]
fn ceiling_carve_stroke_presnaps_onto_the_roof_division() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).expect("amount applies");
    c.materialize();

    let segs = c.ceiling_carve_reference_segments();
    let tol = c.tuning.subarea.corner_snap_m;
    // A carve traced with its left edge ~5 cm to the right of the division
    // (x≈4.05) — within the corner-snap tol of x=4.
    let raw = vec![
        Point2f32::new(4.05, 2.0),
        Point2f32::new(9.0, 2.0),
        Point2f32::new(9.0, 8.0),
        Point2f32::new(4.06, 8.0),
    ];
    let snapped = manip_plan::snap_stroke_to_segments(
        &raw,
        &segs,
        tol,
        manip_plan::SnapKind::ToReferenceSegment,
    );
    // The two near-division vertices moved exactly ONTO x=4.
    assert!(
        (snapped[0].x - 4.0).abs() < 1e-3,
        "near vertex snapped to division: {:?}",
        snapped[0]
    );
    assert!(
        (snapped[3].x - 4.0).abs() < 1e-3,
        "near vertex snapped to division: {:?}",
        snapped[3]
    );
    // The far vertices (x=9, well past tol) are untouched.
    assert_eq!(snapped[1], raw[1], "far vertex untouched");
    assert_eq!(snapped[2], raw[2], "far vertex untouched");
}

/// END-TO-END: a ceiling-exposure CARVE traced with one edge slightly off the
/// interior roof division (x≈4.05) lands its carved-region edge exactly ON the
/// division (x=4) — proving the pre-snap survives `carve_or_cut` (whose
/// whole-building outline alignment can't reach an interior division). The
/// in-flight carve polygon is inspected right after the stroke, before
/// dimensioning, so it reflects the snapped+carved ring.
#[test]
fn ceiling_exposure_carve_snaps_to_roof_division_through_carve_or_cut() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).expect("amount applies");
    c.set_ceiling_step_mode(false);
    c.materialize();

    // Enter the ceiling-exposure tool, clear the storey-count gate, pick Carve.
    c.set_ceiling_exposure_mode(true);
    c.confirm_all_storeys_added_ceiling();
    c.set_ceiling_exposure_select_mode(Some(SelectMode::Carve));

    // Carve a rectangle whose LEFT edge runs ~5 cm right of the division at x=4
    // (within corner-snap tol), the other edges well clear of it.
    c.apply_ceiling_exposure_carve_stroke(&[
        (4.05, 2.0),
        (8.0, 2.0),
        (8.0, 8.0),
        (4.06, 8.0),
        (4.05, 2.1),
    ]);

    let poly = c.pending_ceiling_exposure_carve_polygon();
    assert!(poly.len() >= 4, "a carve is in flight: {poly:?}");
    // The carved region's LEFT edge sits at x=4 (snapped to the division), not
    // at the drawn ~4.05.
    let min_x = poly.iter().map(|(x, _)| *x).fold(f64::INFINITY, f64::min);
    assert!(
        (min_x - 4.0).abs() < 1e-2,
        "carved edge landed on the division x=4 (got {min_x}): {poly:?}"
    );
}

// ── External photos ─────────────────────────────────────────────────────────

/// Adding a photo for each of the four sides completes the external-photos gate
/// and turns the "External photos" checklist row green; missing one leaves it
/// red. Re-adding a side REPLACES rather than duplicating.
#[test]
fn external_photos_gate_and_checklist_row() {
    use ber_model::ber::common::ExternalSide;
    let mut c = dimensioned_square_controller();
    // Capture recompresses/copies into the photo store, so point it at a temp
    // dir and feed real source files (`Full` keeps bytes verbatim — no need for
    // a valid image).
    let tmp = std::env::temp_dir().join(format!("ber_ext_photos_{}", ItemId::default().0));
    c.set_photos_dir(tmp.clone());
    let src = std::env::temp_dir().join(format!("ber_ext_src_{}.jpg", ItemId::default().0));
    std::fs::write(&src, b"jpeg-bytes").unwrap();

    // Row present and starts red (no photos yet).
    assert!(
        c.stage_a_checklist()
            .iter()
            .any(|it| it.label == "External photos"),
        "External photos row present"
    );
    assert_eq!(
        checklist_status_of(&c, "External photos"),
        ChecklistStatus::Incomplete,
        "no photos yet"
    );
    assert!(!c.external_photos_complete());

    // Three of four sides → still incomplete, mapping not complete.
    c.add_external_photo(ExternalSide::Front, &src, PhotoQuality::Full);
    c.add_external_photo(ExternalSide::Right, &src, PhotoQuality::Full);
    c.add_external_photo(ExternalSide::Left, &src, PhotoQuality::Full);
    assert!(!c.external_photos_complete(), "three sides is not enough");
    assert_eq!(
        checklist_status_of(&c, "External photos"),
        ChecklistStatus::Incomplete
    );
    assert!(!c.mapping_complete(), "missing a side blocks Mapping");

    // The fourth side completes the gate → row green.
    c.add_external_photo(ExternalSide::Back, &src, PhotoQuality::Full);
    assert!(c.external_photos_complete(), "all four sides present");
    assert_eq!(
        checklist_status_of(&c, "External photos"),
        ChecklistStatus::AllStoreys
    );

    // The stored Front photo's path before a re-shoot (a copy in the store, not
    // the source path).
    let front_before = c
        .external_photos()
        .into_iter()
        .find(|(s, _)| *s == ExternalSide::Front)
        .and_then(|(_, v)| v)
        .unwrap()
        .file_path;
    assert!(
        std::path::Path::new(&front_before).starts_with(&tmp),
        "external photo stored under the photos dir, not the source path"
    );

    // Re-adding a side REPLACES (no duplicate) — still four photos, new stored copy.
    c.add_external_photo(ExternalSide::Front, &src, PhotoQuality::Full);
    let externals: Vec<_> = c
        .current_record
        .as_ref()
        .unwrap()
        .data
        .photos
        .iter()
        .filter(|p| p.side.is_some())
        .collect();
    assert_eq!(externals.len(), 4, "re-shoot replaces, no duplicate");
    let front_after = externals
        .iter()
        .find(|p| p.side == Some(ExternalSide::Front))
        .unwrap()
        .file_path
        .clone();
    assert_ne!(
        front_after, front_before,
        "Front re-stored under a fresh path"
    );

    // The view data carries all four in ALL order, each with a stored photo.
    let view = c.external_photos();
    assert_eq!(view.len(), 4);
    assert_eq!(view[0].0, ExternalSide::Front);
    assert!(view[0].1.is_some());

    // Remove a side → gate fails again.
    c.remove_external_photo(ExternalSide::Back);
    assert!(!c.external_photos_complete());
    assert_eq!(
        checklist_status_of(&c, "External photos"),
        ChecklistStatus::Incomplete
    );

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&src);
}

/// External photos round-trip through persistence: they live on `Ber.photos`,
/// so the existing autosave/persist path writes them and a reload restores them.
#[test]
fn external_photos_persist_and_reload() {
    use ber_model::ber::common::ExternalSide;
    let mut c = dimensioned_square_controller();
    let tmp = std::env::temp_dir().join(format!("ber_ext_persist_{}", ItemId::default().0));
    c.set_photos_dir(tmp.clone());
    let src = std::env::temp_dir().join(format!("ber_ext_psrc_{}.jpg", ItemId::default().0));
    std::fs::write(&src, b"jpeg-bytes").unwrap();
    for side in ExternalSide::ALL {
        c.add_external_photo(side, &src, PhotoQuality::Full);
    }
    c.flush_if_dirty().unwrap();
    let id = c.current_record.as_ref().unwrap().id;
    c.open_ber(&id).unwrap();
    assert!(
        c.external_photos_complete(),
        "all four external photos survive a reload"
    );

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&src);
}

// ── Stage-1 (Mapping) review ─────────────────────────────────────────────────

/// Confirming the review sets the durable `mapping_reviewed` flag (the Structure
/// gate), closes the review, and persists across a save/reload.
#[test]
fn mapping_review_confirm_sets_reviewed_flag_and_persists() {
    let mut c = dimensioned_square_controller();
    c.materialize();
    assert!(!c.mapping_reviewed(), "starts un-reviewed");

    c.set_mapping_review_mode(true);
    assert!(c.mapping_review_mode());
    c.confirm_mapping_review();
    assert!(c.mapping_reviewed(), "confirm marks reviewed");
    assert!(!c.mapping_review_mode(), "confirm closes the review");

    // Survives a save + reload.
    c.flush_if_dirty().unwrap();
    let id = c.current_record.as_ref().unwrap().id;
    c.open_ber(&id).unwrap();
    assert!(c.mapping_reviewed(), "reviewed flag persists across reload");
}

/// Confirming the Structure review records the durable `structure_reviewed` flag,
/// closes the review, and persists across a save/reload.
#[test]
fn structure_review_confirm_sets_reviewed_flag_and_persists() {
    let mut c = dimensioned_square_controller();
    c.materialize();
    assert!(!c.structure_reviewed(), "starts un-reviewed");

    c.set_structure_review_mode(true);
    assert!(c.structure_review_mode());
    c.confirm_structure_review();
    assert!(c.structure_reviewed(), "confirm marks reviewed");
    assert!(!c.structure_review_mode(), "confirm closes the review");

    c.flush_if_dirty().unwrap();
    let id = c.current_record.as_ref().unwrap().id;
    c.open_ber(&id).unwrap();
    assert!(
        c.structure_reviewed(),
        "reviewed flag persists across reload"
    );
}

/// The Structure review enumerates a review item per construction group / window
/// type (whole house), each highlighting its surfaces; the cursor wraps.
#[test]
fn structure_review_items_cover_groups_and_cursor_wraps() {
    use ber_model::ber::fenestration::{Frame, Glazing};
    use ber_model::ber::structural::{ConstructionMakeup, ConstructionType, WallConstructionType};

    let mut c = dimensioned_square_controller();
    c.materialize();

    // Attribute all walls to one timber construction → one wall group.
    attribute_all_surfaces(
        &mut c,
        SurfaceKind::Walls,
        ConstructionType::Wall(WallConstructionType::TimberConstruction),
        ConstructionMakeup::Default,
    );

    // Add a window of a registered type → one window-type group.
    let wall_id = c.current_record.as_ref().unwrap().data.storeys[0]
        .walls
        .regions[0]
        .id;
    let type_id = c.current_record.as_mut().unwrap().data.ensure_window_type(
        Glazing::Double,
        Frame::WoodUPVC,
        0,
        false,
        Vec::new(),
        Some("Model A".to_string()),
    );
    ber_model::ber::openings::add_wall_window(
        &mut c.current_record.as_mut().unwrap().data.storeys[0],
        wall_id,
        1.0,
        1.0,
        1.0,
        0.9,
        type_id,
    )
    .expect("window placed");
    c.materialize();

    c.set_structure_review_mode(true);
    let total = c.structure_review_view().expect("has review items").total;
    assert!(total >= 2, "at least a wall group + a window-type group");

    // Walk every item: a wall group and a window-type group are present, each
    // highlighting ≥1 surface.
    let (mut saw_wall, mut saw_window) = (false, false);
    for _ in 0..total {
        let v = c.structure_review_view().unwrap();
        assert!(
            !v.highlight.is_empty(),
            "every item highlights its surfaces"
        );
        match v.kind {
            StructureReviewKind::Wall => saw_wall = true,
            StructureReviewKind::WindowType => saw_window = true,
            _ => {}
        }
        c.step_structure_review(1);
    }
    assert!(saw_wall, "a wall construction group is reviewed");
    assert!(saw_window, "a window-type group is reviewed");
    // A full lap of `total` steps returns to the first item.
    assert_eq!(c.structure_review_view().unwrap().cursor, 0, "cursor wraps");
}

/// Phase-3 objects: tapping drops/opens a room pin, objects accumulate (summing
/// counts), a near tap reopens the same pin (no duplicate), a far tap makes a
/// new one, and undo removes it.
#[test]
fn objects_capture_pins_counts_and_undo() {
    use ber_model::ber::objects::{BoilerFuel, ObjectKind, RadiatorTrv};

    let mut c = dimensioned_square_controller();
    c.materialize();

    // Drop a pin and add two object entries (1 boiler + 5 radiators = 6).
    c.objects_pointer_down(2.0, 2.0, 0.5);
    let room = c.active_object_room().expect("a room opened");
    c.add_object(
        ObjectKind::Boiler(BoilerFuel::Gas),
        1,
        None,
        Vec::new(),
        PhotoQuality::Full,
    );
    c.add_object(
        ObjectKind::Radiator(RadiatorTrv::WithTrv),
        5,
        None,
        Vec::new(),
        PhotoQuality::Full,
    );
    let pins = c.object_rooms_view();
    assert_eq!(pins.len(), 1, "one pin");
    assert_eq!(pins[0].count, 6, "1 boiler + 5 radiators");
    let av = c.active_object_room_view().expect("room open");
    assert_eq!(av.objects.len(), 2, "two object entries");

    // Closing then tapping NEAR reopens the same pin (no duplicate).
    c.close_object_room();
    assert!(c.active_object_room().is_none());
    c.objects_pointer_down(2.1, 2.1, 0.5);
    assert_eq!(c.active_object_room(), Some(room), "near tap reopens it");
    assert_eq!(c.object_rooms_view().len(), 1, "no new pin");

    // Tapping FAR drops a second pin; undo removes it.
    c.objects_pointer_down(8.0, 8.0, 0.5);
    assert_eq!(c.object_rooms_view().len(), 2, "a far tap adds a pin");
    c.undo().expect("undo");
    assert_eq!(c.object_rooms_view().len(), 1, "undo removed the new pin");
}

/// Phase-3 objects placed from a 3D view: the resolved surface tap stores the
/// pin ON the surface (storey + height + the sloped surface's pitch), the open
/// room works even from the full-house view (no current storey), the panel
/// view exposes the pitch for the dialog, the pin feeds the 3D hotspot views
/// at its surface height, a near 3D tap reopens the same pin, and undo removes
/// a dropped pin.
#[test]
fn objects_3d_placement_stores_surface_point_and_pitch() {
    use ber_model::ber::objects::{BoilerFuel, ObjectKind};

    let mut c = dimensioned_square_controller();
    c.materialize();
    // The full-house 3D is reached from the All tab: no current storey.
    c.set_current_storey(None).expect("All tab");

    // A pick on storey 0's roof plane at (2, 3), 2.3 m up, pitched 35°.
    c.objects_pointer_down_3d(0, 2.0, 3.0, 2.3, Some(35.0), 0.5);
    let room = c
        .active_object_room()
        .expect("a room opened on the surface");
    let av = c
        .active_object_room_view()
        .expect("panel opens without a current storey");
    assert_eq!(av.id, room);
    assert_eq!(
        av.surface_pitch_deg,
        Some(35.0),
        "the dialog sees the pitch"
    );

    // The room accepts objects while no storey is current.
    c.add_object(
        ObjectKind::Boiler(BoilerFuel::Gas),
        1,
        None,
        Vec::new(),
        PhotoQuality::Full,
    );
    assert_eq!(
        c.active_object_room_view().unwrap().objects.len(),
        1,
        "object recorded on the 3D-placed pin"
    );

    // The pin renders at its surface height in the 3D views (world z = storey
    // 0's datum 0 + 2.3).
    let world = c.object_pin_world_points();
    assert_eq!(world.len(), 1);
    assert!((world[0].0 - 2.0).abs() < 1e-6 && (world[0].1 - 3.0).abs() < 1e-6);
    assert!(
        (world[0].2 - 2.3).abs() < 1e-6,
        "hotspot at the surface height, not the legacy 1 m (z={})",
        world[0].2
    );

    // Back on the storey tab, the pin view carries the height too.
    c.set_current_storey(Some(0)).expect("storey 0");
    let pins = c.object_rooms_view();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].z_m, Some(2.3));

    // A near 3D tap (same surface point) reopens the same pin — no duplicate.
    c.close_object_room();
    c.objects_pointer_down_3d(0, 2.1, 3.0, 2.35, None, 0.5);
    assert_eq!(c.active_object_room(), Some(room), "near tap reopens it");
    assert_eq!(c.object_rooms_view().len(), 1, "no new pin");

    // A far 3D tap drops a fresh pin (no pitch on a wall pick); undo removes it.
    c.objects_pointer_down_3d(0, 8.0, 8.0, 1.2, None, 0.5);
    assert_eq!(c.object_rooms_view().len(), 2);
    assert_eq!(
        c.active_object_room_view().unwrap().surface_pitch_deg,
        None,
        "a wall placement carries no pitch"
    );
    c.undo().expect("undo");
    assert_eq!(c.object_rooms_view().len(), 1, "undo removed the 3D pin");
}

/// The Objects review sums counts house-wide (flat list) and locates the
/// selected kind in 3D (one hotspot per room holding it).
#[test]
fn object_review_totals_and_hotspots() {
    use ber_model::ber::objects::{BoilerFuel, ObjectKind, RadiatorTrv};

    let mut c = dimensioned_square_controller();
    c.materialize();
    // Room 1: 1 gas boiler + 3 radiators-with-TRV.
    c.objects_pointer_down(2.0, 2.0, 0.5);
    c.add_object(
        ObjectKind::Boiler(BoilerFuel::Gas),
        1,
        None,
        Vec::new(),
        PhotoQuality::Full,
    );
    c.add_object(
        ObjectKind::Radiator(RadiatorTrv::WithTrv),
        3,
        None,
        Vec::new(),
        PhotoQuality::Full,
    );
    c.close_object_room();
    // Room 2: 2 more radiators-with-TRV.
    c.objects_pointer_down(8.0, 8.0, 0.5);
    c.add_object(
        ObjectKind::Radiator(RadiatorTrv::WithTrv),
        2,
        None,
        Vec::new(),
        PhotoQuality::Full,
    );
    c.close_object_room();

    c.set_object_review_mode(true);
    let rv = c.object_review_view().expect("has totals");
    let rad_idx = rv
        .totals
        .iter()
        .position(|t| t.label.contains("With TRV"))
        .expect("radiator row");
    assert_eq!(rv.totals[rad_idx].count, 5, "3 + 2 radiators");
    assert_eq!(
        rv.totals
            .iter()
            .find(|t| t.label.contains("Boiler"))
            .unwrap()
            .count,
        1
    );

    // Selecting the radiator row locates it in both rooms (two hotspots).
    c.select_object_review(rad_idx);
    assert_eq!(
        c.object_review_view().unwrap().hotspots.len(),
        2,
        "radiators in two rooms → two hotspots"
    );
}

/// Phase-4 form + sheltering setters write to the record, sheltering syncs the
/// legacy count, and confirm marks the BER's final review.
#[test]
fn final_phase_form_sheltering_and_confirm() {
    use ber_model::ber::common::ExternalSide;
    use ber_model::ber::final_details::{FloorKind, HeatingKind, ShelterCategory, WallFinish};

    let mut c = dimensioned_square_controller();
    c.materialize();

    c.set_final_floor(FloorKind::SuspendedSealed);
    c.set_final_wall_finish(WallFinishField::External, WallFinish::HardPlaster);
    c.set_final_heating(HeatingKind::Both);
    let d = c.final_details();
    assert_eq!(d.floor, FloorKind::SuspendedSealed);
    assert_eq!(d.external_wall, WallFinish::HardPlaster);
    assert_eq!(d.heating, HeatingKind::Both);

    // Cycling a side advances its category and syncs the sheltered count.
    c.cycle_shelter(ExternalSide::Front); // Unset → Sheltered (= 1.0)
    assert_eq!(c.sheltering().front, ShelterCategory::Sheltered);
    c.cycle_shelter(ExternalSide::Right); // → Sheltered (another 1.0)
    let ber = &c.current_record.as_ref().unwrap().data;
    assert!(
        (ber.sheltered_sides - 2.0).abs() < 1e-4,
        "two sheltered sides → count 2.0, got {}",
        ber.sheltered_sides
    );

    // North align: confirming records it (and the checklist/north row reflects it).
    assert!(!c.north_aligned());
    assert!(
        !c.final_complete(),
        "not complete until form + north + all sides"
    );
    c.confirm_align();
    assert!(c.north_aligned(), "align OK records north");

    assert!(!c.details_reviewed());
    assert!(!c.show_ber_complete());
    c.confirm_final_review();
    assert!(c.details_reviewed(), "confirm marks the final review");
    assert!(c.show_ber_complete(), "confirm raises the completion popup");
    // Completion flow: popup → summary → back.
    c.open_summary();
    assert!(
        !c.show_ber_complete() && c.show_summary(),
        "popup → summary"
    );
    c.close_summary();
    assert!(!c.show_summary(), "summary closed");

    // Survives reload.
    c.flush_if_dirty().unwrap();
    let id = c.current_record.as_ref().unwrap().id;
    c.open_ber(&id).unwrap();
    assert!(c.details_reviewed());
    assert_eq!(c.final_details().heating, HeatingKind::Both);
}

/// The review prompts to confirm each EMPTY category one at a time: "Yes"
/// records it absent (durably, drops out), "No" skips it for the session
/// (re-offered on the next review).
#[test]
fn object_review_absence_confirmation() {
    use ber_model::ber::objects::{BoilerFuel, ObjectCategory, ObjectKind};

    let mut c = dimensioned_square_controller();
    c.materialize();
    c.objects_pointer_down(2.0, 2.0, 0.5);
    c.add_object(
        ObjectKind::Boiler(BoilerFuel::Gas),
        1,
        None,
        Vec::new(),
        PhotoQuality::Full,
    );
    c.close_object_room();

    c.set_object_review_mode(true);
    let rv = c.object_review_view().expect("review open");
    let first = rv.absent_prompt.expect("an empty category to confirm");
    assert_ne!(
        first,
        ObjectCategory::Boiler,
        "boiler has one — not prompted"
    );
    let remaining0 = rv.absent_remaining;

    // "Yes — none" records it absent and advances.
    c.confirm_object_absent(first);
    assert!(c
        .current_record
        .as_ref()
        .unwrap()
        .data
        .objects_no_items_confirmed
        .contains(&first));
    let rv2 = c.object_review_view().unwrap();
    assert_eq!(rv2.absent_remaining, remaining0 - 1, "one fewer to confirm");
    assert_ne!(
        rv2.absent_prompt,
        Some(first),
        "advanced past the confirmed one"
    );

    // "No — there is one" skips for the session only.
    let second = rv2.absent_prompt.expect("next empty category");
    c.skip_object_absent(second);
    assert_ne!(
        c.object_review_view().unwrap().absent_prompt,
        Some(second),
        "skipped one advances within the session"
    );

    // Reopening the review clears the session skip → `second` is re-offered,
    // while the confirmed `first` stays recorded (never re-offered).
    c.set_object_review_mode(false);
    c.set_object_review_mode(true);
    assert!(c
        .current_record
        .as_ref()
        .unwrap()
        .data
        .objects_no_items_confirmed
        .contains(&first));
}

/// Stepping the side cursor wraps through the four sides.
#[test]
fn mapping_review_side_cursor_wraps() {
    use ber_model::ber::common::ExternalSide;
    let mut c = dimensioned_square_controller();
    c.materialize();
    c.set_mapping_review_mode(true);
    assert_eq!(c.mapping_review_view().unwrap().side, ExternalSide::Front);
    c.step_mapping_review_side(1);
    assert_eq!(c.mapping_review_view().unwrap().side, ExternalSide::Right);
    c.step_mapping_review_side(-1);
    assert_eq!(c.mapping_review_view().unwrap().side, ExternalSide::Front);
    c.step_mapping_review_side(-1); // wrap past Front → last (Back)
    assert_eq!(c.mapping_review_view().unwrap().side, ExternalSide::Back);
}

/// A drag in the review elevation maps the composite cursor back to the
/// opening's wall and moves it persistently; an out-of-range drag is clamped by
/// the existing `move_opening` constraint (the opening never flies off the wall).
#[test]
fn mapping_review_drag_moves_opening_and_stays_on_wall() {
    use ber_model::ber::common::ExternalSide;
    let mut c = dimensioned_square_controller();
    c.materialize();

    // The +Y (Front) wall is the one whose base midpoint has the greatest y.
    let front_id = {
        let st = &c.current_record.as_ref().unwrap().data.storeys[0];
        st.walls
            .regions
            .iter()
            .max_by(|a, b| {
                a.base_midpoint()
                    .unwrap()
                    .y
                    .partial_cmp(&b.base_midpoint().unwrap().y)
                    .unwrap()
            })
            .unwrap()
            .id
    };
    ber_model::ber::openings::add_wall_window(
        &mut c.current_record.as_mut().unwrap().data.storeys[0],
        front_id,
        5.0,
        1.2,
        1.0,
        0.9,
        ItemId::default(),
    )
    .expect("window placed on the front wall");

    c.set_mapping_review_mode(true);
    c.set_mapping_review_side(ExternalSide::Front);
    let v = c.mapping_review_view().unwrap();
    assert_eq!(v.elevation.panels.len(), 1, "front wall faces Front");
    assert_eq!(v.elevation.openings.len(), 1);
    let o = v.elevation.openings[0];
    let (cy, panel) = (o.sill_z + o.height_m / 2.0, v.elevation.panels[0].clone());

    // Grab the opening and drag its centre +2 m along the view axis.
    c.mapping_review_pointer_down(o.x_center, cy);
    c.mapping_review_pointer_drag(o.x_center + 2.0, cy);
    c.mapping_review_pointer_up();
    let moved = c.mapping_review_view().unwrap().elevation.openings[0];
    assert!(
        (moved.x_center - (o.x_center + 2.0)).abs() < 0.3,
        "opening followed the drag (from {} to {})",
        o.x_center,
        moved.x_center
    );

    // A wild out-of-range drag is constrained: the opening stays within the
    // wall panel rather than flying off.
    c.mapping_review_pointer_down(moved.x_center, cy);
    c.mapping_review_pointer_drag(moved.x_center + 100.0, cy);
    c.mapping_review_pointer_up();
    let clamped = c.mapping_review_view().unwrap().elevation.openings[0];
    assert!(
        clamped.x_center >= panel.x0 - 1e-6 && clamped.x_center <= panel.x1 + 1e-6,
        "opening x {} stayed within wall panel [{}, {}]",
        clamped.x_center,
        panel.x0,
        panel.x1
    );
}

/// Dragging an opening upward in the review elevation can't push it above the
/// wall top — the sill is clamped against the roofline.
#[test]
fn mapping_review_drag_clamps_opening_under_the_top() {
    use ber_model::ber::common::ExternalSide;
    let mut c = dimensioned_square_controller();
    c.materialize();
    let front_id = {
        let st = &c.current_record.as_ref().unwrap().data.storeys[0];
        st.walls
            .regions
            .iter()
            .max_by(|a, b| {
                a.base_midpoint()
                    .unwrap()
                    .y
                    .partial_cmp(&b.base_midpoint().unwrap().y)
                    .unwrap()
            })
            .unwrap()
            .id
    };
    ber_model::ber::openings::add_wall_window(
        &mut c.current_record.as_mut().unwrap().data.storeys[0],
        front_id,
        5.0,
        1.2,
        1.0,
        0.5,
        ItemId::default(),
    )
    .expect("window placed");

    c.set_mapping_review_mode(true);
    c.set_mapping_review_side(ExternalSide::Front);
    let v = c.mapping_review_view().unwrap();
    let o = v.elevation.openings[0];
    let wall_top = v.elevation.max_z;

    // Drag the centre way above the roof.
    c.mapping_review_pointer_down(o.x_center, o.sill_z + o.height_m / 2.0);
    c.mapping_review_pointer_drag(o.x_center, wall_top + 50.0);
    c.mapping_review_pointer_up();

    let moved = c.mapping_review_view().unwrap().elevation.openings[0];
    assert!(
        moved.sill_z + moved.height_m <= wall_top + 1e-6,
        "opening top {} stayed under the wall top {}",
        moved.sill_z + moved.height_m,
        wall_top
    );
}

/// While TRACING a new upper storey over the watermark, the world stroke
/// pre-snaps onto the storey-below's INTERIOR construction-phase boundary —
/// not just onto its outline. A wall drawn ~0.3 m off the phase division at
/// x=5 (well within the ~8%-of-diagonal tolerance, far from any outline wall)
/// must land exactly ON x=5, so the inherited phase seed splits the new floor
/// along the real "year" line instead of a freehand offset.
#[test]
fn outline_world_snaps_to_below_storey_phase_boundary() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10 m

    // Tag the RIGHT half (x >= 5) of storey 0 as Extension1 and materialize so
    // the phase partition reaches the persisted floors — what the upper-storey
    // snap reads back via `document::phase_regions_from_floors`.
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
        phase: ConstructionPhase::Extension1,
    });
    c.mark_dirty().unwrap();
    c.materialize();
    let data = &c.current_record.as_ref().unwrap().data;
    assert_eq!(
        document::phase_regions_from_floors(data, 0).len(),
        1,
        "sanity: the tagged zone is reconstructable from the persisted floors"
    );
    // The snap references keep ONLY the interior division (both ends touch the
    // outline but the middle crosses the interior); the zone edges running
    // along the outline are filtered out (the carve owns those).
    let prev_ring: Vec<Point2f32> = vec![p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)];
    let segs = document::phase_snap_segments(data, 0, &prev_ring);
    assert_eq!(
        segs.len(),
        1,
        "only the interior division survives: {segs:?}"
    );
    assert!(
        segs[0].iter().all(|q| (q.x - 5.0).abs() < 1e-3),
        "the division runs along x=5: {segs:?}"
    );

    c.add_storey().unwrap();
    // Trace the LEFT half with the right wall drawn ~0.3 m past the division.
    let world = vec![(0.2, 0.2), (5.3, 0.2), (5.3, 9.8), (0.2, 9.8), (0.3, 0.3)];
    c.apply_outline_world_inner(&world, true)
        .expect("carve applies");
    assert_eq!(c.document.floor_plan.ring_count(), 1, "outline built");
    let pts = c.document.floor_plan.get_points();
    let max_x = pts.iter().map(|v| v.x).fold(f32::MIN, f32::max);
    assert!(
        (max_x - 5.0).abs() < 1e-2,
        "right wall landed ON the phase boundary x=5 (got {max_x}): {pts:?}"
    );
    // And the footprint is the left half, not the drawn 5.3-wide sliver.
    let area = ring_area(&c);
    assert!((area - 50.0).abs() < 2.0, "footprint ≈ 50 m² (got {area})");
}

/// PROBE (user repro): add storey → draw → delete → add again → accept
/// thickness must land back on the draw-vs-suggest CHOICE, with suggestions
/// still offerable.
#[test]
fn probe_delete_readd_reopens_choose_method() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10 m
    c.set_default_ceiling_height(2.4).unwrap();
    c.add_storey().unwrap();
    let p1 = c.new_storey_prompt().expect("flow open on first add");
    assert!(p1.thickness_pending, "first add asks thickness");
    c.assign_floor_thickness(0.3).unwrap();
    let p2 = c.new_storey_prompt().expect("flow open after thickness");
    assert!(p2.method_pending, "first add asks the method");
    assert!(p2.can_suggest, "suggestions available on first add");
    // Draw the outline (implicit choose_draw): full world entry point.
    c.apply_outline_stroke_world(&[(0.2, 0.2), (9.8, 0.2), (9.8, 9.8), (0.2, 9.8), (0.3, 0.3)]);
    assert!(
        c.new_storey_prompt().is_none(),
        "drawing closed the flow (implicit draw choice)"
    );
    // Delete it and add again — the user's sequence.
    c.delete_current_storey().unwrap();
    c.add_storey().unwrap();
    let p3 = c
        .new_storey_prompt()
        .expect("flow open on second add (thickness)");
    assert!(p3.thickness_pending, "second add asks thickness again");
    c.assign_floor_thickness(0.3).unwrap();
    let p4 = c
        .new_storey_prompt()
        .expect("flow STILL open after second thickness accept");
    assert!(
        p4.method_pending,
        "second add must re-ask draw-vs-suggest, not assume draw"
    );
    assert!(
        p4.can_suggest,
        "suggestions must still be offerable after delete + re-add"
    );
}

/// Dimensioning must not re-ask for a wall that pre-snapped onto the
/// storey-below's construction-phase boundary: a snapped wall is a known wall
/// (the same rule the living-area flow applies via its reference walls), so a
/// left-half trace whose right wall lands ON the x=5 phase division and whose
/// other walls lie on the previous outline has NOTHING left to dimension — no
/// wall prompt opens.
#[test]
fn outline_world_phase_snapped_wall_is_pinned_pre_known() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10 m
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
        phase: ConstructionPhase::Extension1,
    });
    c.mark_dirty().unwrap();
    c.materialize();
    c.add_storey().unwrap();
    // Left half: the right wall drawn ~0.3 m past the division, the other
    // three walls hugging the previous outline.
    let world = vec![(0.2, 0.2), (5.3, 0.2), (5.3, 9.8), (0.2, 9.8), (0.3, 0.3)];
    c.apply_outline_world_inner(&world, true)
        .expect("carve applies");
    let pts = c.document.floor_plan.get_points();
    let max_x = pts.iter().map(|v| v.x).fold(f32::MIN, f32::max);
    assert!(
        (max_x - 5.0).abs() < 1e-2,
        "right wall snapped onto the phase boundary x=5: {pts:?}"
    );
    // BOTH axes' walls have known coordinates (previous outline + phase
    // line): every active param is pinned, so no dimension prompt opens —
    // the log's "pinned N of N".
    let active = c.document.floor_plan.all_active_params();
    assert!(
        active.is_subset(&c.document.dimensioning.known_params),
        "every wall coordinate pre-known: active {active:?} vs known {:?}",
        c.document.dimensioning.known_params
    );
    assert!(
        c.document.dimensioning.current_prompt.is_none(),
        "a wall snapped onto a phase line must not be re-asked"
    );
}

/// Declining the phase-boundary snap tag honestly REOPENS the dimension
/// question: "Don't snap" replays the SAME world stroke through
/// `apply_outline_world_inner`, the formerly-snapped wall no longer lies on
/// the phase line, so its param is NOT pinned and a wall prompt IS open.
#[test]
fn outline_world_unsnap_phase_boundary_reopens_dimension_prompt() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10 m
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
        phase: ConstructionPhase::Extension1,
    });
    c.mark_dirty().unwrap();
    c.materialize();
    c.add_storey().unwrap();
    // The full entry point (capture + tags) gates on the slab thickness and
    // needs an engine stroke for the close-check; the world points below are
    // what get applied.
    c.assign_floor_thickness(0.3).unwrap();
    c.sketch_start_stroke(Point2f32::new(0.0, 0.0)).unwrap();
    for (x, y) in [(100.0, 0.0), (100.0, 100.0), (0.0, 100.0), (1.0, 1.0)] {
        c.sketch_add_point(Point2f32::new(x, y)).unwrap();
    }
    let world = vec![(0.2, 0.2), (5.3, 0.2), (5.3, 9.8), (0.2, 9.8), (0.3, 0.3)];
    c.apply_outline_stroke_world(&world);
    assert!(
        c.document.dimensioning.current_prompt.is_none(),
        "while snapped, nothing is left to ask"
    );
    // Decline EVERY phase-boundary snap. Each decline replays the stroke and
    // re-captures fresh tags, so re-fetch the overlay after each one.
    let mut declined = 0;
    while let Some(tag) = c
        .snap_overlay()
        .into_iter()
        .find(|t| t.current_kind == Some(manip_plan::SnapKind::ToPhaseBoundary))
    {
        c.choose_snap(Point2f32::new(tag.from.0, tag.from.1), None);
        declined += 1;
        assert!(declined <= 8, "un-snap loop did not converge");
    }
    assert!(declined > 0, "the phase snap surfaced as a clickable tag");
    // The un-snapped wall sits at its drawn ~5.3, OFF the phase line — its
    // coordinate is genuinely unknown again.
    let pts = c.document.floor_plan.get_points();
    let max_x = pts.iter().map(|v| v.x).fold(f32::MIN, f32::max);
    assert!(
        max_x > 5.05,
        "declined snap leaves the wall off the phase line: {pts:?}"
    );
    let active = c.document.floor_plan.all_active_params();
    assert!(
        !active.is_subset(&c.document.dimensioning.known_params),
        "the un-snapped wall's param is no longer pinned"
    );
    assert!(
        c.document.dimensioning.current_prompt.is_some(),
        "the dimension question honestly returns after \"Don't snap\""
    );
}

// ----- delete storey (restart a badly-drawn storey) -------------------------

/// `delete_current_storey` refuses everywhere except on the TOP storey
/// (index >= 1): no BER loaded, the ground storey, a mid-stack storey, and
/// the All tab all leave the storeys intact.
#[test]
fn delete_storey_refusals() {
    // No BER loaded.
    let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
    assert!(c.delete_current_storey().is_err(), "no BER loaded");

    // Ground storey (also the only storey).
    let mut c = dimensioned_square_controller();
    assert!(c.delete_current_storey().is_err(), "storey 0 is protected");
    assert_eq!(c.current_record.as_ref().unwrap().data.storeys.len(), 1);
    assert_eq!(c.document.storeys.len(), 1);

    // Mid-stack storey: storeys 0,1,2 with the editor on 1.
    c.add_storey().unwrap();
    finalise_current_storey_as_square(&mut c, 6.0);
    c.add_storey().unwrap();
    finalise_current_storey_as_square(&mut c, 4.0);
    c.set_current_storey(Some(1)).unwrap();
    assert!(
        c.delete_current_storey().is_err(),
        "mid-stack storey refused (only the top storey deletes)"
    );
    assert_eq!(c.current_record.as_ref().unwrap().data.storeys.len(), 3);
    assert_eq!(c.document.storeys.len(), 3);

    // All-storeys tab — no current storey to delete.
    c.set_current_storey(None).unwrap();
    assert!(c.delete_current_storey().is_err(), "All tab has no storey");
    assert_eq!(c.current_record.as_ref().unwrap().data.storeys.len(), 3);

    // The document model itself refuses to drop its last storey (the
    // never-empty invariant `Deref` relies on).
    let mut d = document::Document::default();
    assert!(!d.remove_last_storey());
    assert_eq!(d.storeys.len(), 1);
}

/// Deleting the top storey shrinks BOTH parallel containers in lock-step,
/// clears every piece of per-storey session state keyed by the dead index,
/// and lands the editor on the storey below with its geometry intact.
#[test]
fn delete_top_storey_shrinks_and_lands_below() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10
    c.add_storey().unwrap();
    finalise_current_storey_as_square(&mut c, 6.0); // storey 1 = 6x6
    assert_eq!(c.current_storey_idx(), Some(1));

    // Plant per-storey session state keyed by the doomed index so the delete
    // provably clears it (and storey 0's own entries provably survive).
    let key = document::centroid_key(domain::points::Point2f64::new(3.0, 3.0));
    c.inherited_seed_pending.insert(1);
    c.floor_exposure_selected.insert((1, key));
    c.roof_selected.insert((1, key));
    c.ceiling_exposure_selected.insert((0, key));
    c.ceiling_exposure_selected.insert((1, key));

    c.delete_current_storey().expect("top storey deletes");

    assert_eq!(c.current_record.as_ref().unwrap().data.storeys.len(), 1);
    assert_eq!(c.document.storeys.len(), 1, "lock-step shrink");
    assert_eq!(
        c.current_storey_idx(),
        Some(0),
        "landed on the storey below"
    );
    assert!(!c.phase_regions.contains_key(&1));
    assert!(!c.step_regions.contains_key(&1));
    assert!(!c.ceiling_step_regions.contains_key(&1));
    assert!(!c.inherited_seed_pending.contains(&1));
    assert!(!c.floor_exposure_selected.iter().any(|(i, _)| *i == 1));
    assert!(!c.roof_selected.iter().any(|(i, _)| *i == 1));
    assert!(!c.ceiling_exposure_selected.iter().any(|(i, _)| *i == 1));
    assert!(
        c.ceiling_exposure_selected.contains(&(0, key)),
        "surviving storeys' selections stay"
    );
    // The editor shows storey 0's finalised outline again (lossless — the
    // StoreyDoc was preserved in place).
    assert_eq!(c.document.floor_plan.ring_count(), 1);
    assert!(c.document.dimensioning.scale_set);
    assert!(c.pending_new_storey.is_none(), "no stale new-storey flow");
    assert!(
        !c.can_undo(),
        "a deleted storey is outside the editing history"
    );
    assert!(c.redo.is_empty());
    // The shrunken record still derives cleanly.
    c.commit_to_ber().expect("commit after delete");
    assert_eq!(c.current_record.as_ref().unwrap().data.storeys.len(), 1);
    assert!(!c.current_record.as_ref().unwrap().data.storeys[0]
        .floors
        .to_vertex_lists()
        .is_empty());
}

/// Delete-then-re-add (the "start again" round trip): the new top storey is
/// fresh — empty outline, scale uncalibrated — and the new-storey flow
/// re-opens at the thickness question.
#[test]
fn delete_then_readd_storey_starts_fresh() {
    let mut c = dimensioned_square_controller();
    c.add_storey().unwrap();
    finalise_current_storey_as_square(&mut c, 6.0); // the "bad" drawing
    c.delete_current_storey().unwrap();

    c.add_storey().expect("re-add after delete");
    assert_eq!(c.current_storey_idx(), Some(1));
    assert_eq!(c.current_record.as_ref().unwrap().data.storeys.len(), 2);
    assert_eq!(c.document.storeys.len(), 2);
    assert_eq!(
        c.document.floor_plan.ring_count(),
        0,
        "fresh empty storey — the deleted 6x6 is gone"
    );
    assert!(!c.document.dimensioning.scale_set);
    let prompt = c.new_storey_prompt().expect("new-storey flow re-opens");
    assert!(prompt.thickness_pending, "thickness asked first");
    assert!(c.inherited_seed_pending.contains(&1));
}

/// CEILING SPLIT AT NEW-STOREY FINALISATION: a new storey whose footprint
/// PARTIALLY covers the below storey's flat ceiling splits that ceiling in two
/// along the footprint boundary — the seed tail's commit runs the pipeline's
/// `derive_inter_storey`, which splits (regions-as-truth) and stamps the
/// covered piece `InterStorey`; the uncovered piece stays separately
/// classifiable. DERIVED-ONLY: nothing is pushed onto `ceiling_splits`.
/// Re-running the tail (a redraw re-pends the seed) is idempotent — no extra
/// ceiling pieces.
#[test]
fn new_storey_partial_cover_derives_inter_storey_ceiling_piece() {
    use ber_model::ber::structural::CeilingExposure;
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).expect("height applies");
    c.materialize();
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .ceilings
            .regions
            .len(),
        1,
        "one flat ceiling below before the new storey"
    );

    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    // Drawn-path finalisation: adopt a FINALISED 4×10 outline covering the
    // left strip of the 10×10 below, then fire the seed tail (the same entry
    // `accept_suggestion` and the dimensioning/carve tails funnel into).
    let ring = vec![p(0., 0.), p(4., 0.), p(4., 10.), p(0., 10.)];
    c.document.adopt_finalised_outline(&ring).expect("adopts");
    c.maybe_seed_inherited_levels();
    assert!(!c.inherited_seed_pending.contains(&1), "seed consumed");

    {
        let below = &c.current_record.as_ref().unwrap().data.storeys[0];
        assert!(
            below.ceiling_splits.is_empty(),
            "derived-only — no carve ring pushed onto the below storey"
        );
        assert_eq!(
            below.ceilings.regions.len(),
            2,
            "the below ceiling split in two along the footprint boundary"
        );
        let covered = below
            .ceilings
            .regions
            .iter()
            .find(|r| r.centroid().x < 4.0)
            .expect("the piece under the new footprint exists");
        let uncovered = below
            .ceilings
            .regions
            .iter()
            .find(|r| r.centroid().x > 4.0)
            .expect("the uncovered piece exists");
        assert!(
            (covered.area() - 40.0).abs() < 1e-3 && (uncovered.area() - 60.0).abs() < 1e-3,
            "split along x = 4: {} / {}",
            covered.area(),
            uncovered.area()
        );
        assert_eq!(
            covered.exposure,
            CeilingExposure::InterStorey,
            "the covered piece derives InterStorey"
        );
        assert_ne!(
            uncovered.exposure,
            CeilingExposure::InterStorey,
            "the uncovered piece is NOT InterStorey — left for the assessor"
        );
    }

    // Re-run the tail as a redraw would (the seed re-pends, the same
    // footprint finalises again): the derivation is idempotent — no extra
    // ceiling pieces, still nothing durable pushed.
    c.inherited_seed_pending.insert(1);
    c.maybe_seed_inherited_levels();
    let below = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(below.ceiling_splits.is_empty(), "still nothing durable");
    assert_eq!(
        below.ceilings.regions.len(),
        2,
        "still two ceiling pieces after the re-run"
    );
}

/// Build a two-storey stack whose upper storey PARTIALLY covers the below
/// storey's flat ceiling (the 4×10 left strip of a 10×10, the
/// `new_storey_partial_cover_derives_inter_storey_ceiling_piece` fixture):
/// after finalisation the below ceiling is split in two with the covered piece
/// derived `InterStorey`. The starting point for the delete/redraw RETRACTION
/// tests.
fn partial_cover_two_storey_controller() -> AppController {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10
    c.set_default_ceiling_height(2.4).expect("height applies");
    c.materialize();
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    let ring = vec![p(0., 0.), p(4., 0.), p(4., 10.), p(0., 10.)];
    c.document.adopt_finalised_outline(&ring).expect("adopts");
    c.maybe_seed_inherited_levels();
    // Sanity — the split + InterStorey piece are in place (the classification
    // the retraction must undo).
    let below = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(below.ceiling_splits.is_empty(), "fixture: derived-only");
    assert_eq!(below.ceilings.regions.len(), 2, "fixture: ceiling split");
    assert!(
        below
            .ceilings
            .regions
            .iter()
            .any(|r| r.exposure == ber_model::ber::structural::CeilingExposure::InterStorey),
        "fixture: the covered piece derived InterStorey"
    );
    c
}

/// DELETING the partially-covering top storey RETRACTS its classification
/// from the storey below: the commit's `derive_inter_storey` clear pass
/// un-stamps `InterStorey` (no storey above → nothing re-stamps), so every
/// piece is classifiable again. The SPLIT pieces themselves may remain —
/// ceilings are regions-as-truth and the derivation's split is durable there
/// (accepted: both pieces are Unset and the assessor can label each). Nothing
/// was ever pushed onto `ceiling_splits`, so there's nothing durable to drop.
#[test]
fn delete_storey_retracts_inter_storey_ceiling_classification() {
    use ber_model::ber::structural::CeilingExposure;
    let mut c = partial_cover_two_storey_controller();

    c.delete_current_storey().expect("top storey deletes");

    let below = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(below.ceiling_splits.is_empty(), "still nothing durable");
    assert!(
        below
            .ceilings
            .regions
            .iter()
            .all(|r| r.exposure != CeilingExposure::InterStorey),
        "no resolved region reads InterStorey (all classifiable again)"
    );
    // The retraction survives the next derive cycle (nothing re-stamps —
    // there is no storey above anymore).
    c.commit_to_ber()
        .expect("commit stays green after the delete");
    let below = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(below
        .ceilings
        .regions
        .iter()
        .all(|r| r.exposure != CeilingExposure::InterStorey));
}

/// A WHOLESALE REDRAW of the upper storey's outline: the redraw reset (what
/// `apply_drawperp_stroke` / `apply_outline_world_inner` call right after
/// replacing the outline) re-pends the seed, and the re-finalisation against
/// a DIFFERENT partial footprint re-derives the classification — the clear
/// pass retracts the OLD boundary's `InterStorey` stamp and only the NEW
/// covered piece classifies. The old boundary's split pieces may remain
/// (regions-as-truth residue, accepted — they read Unset and stay
/// labellable); no durable rings are involved.
#[test]
fn redraw_rederives_inter_storey_against_the_new_footprint_only() {
    use ber_model::ber::structural::CeilingExposure;
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = partial_cover_two_storey_controller(); // old cover: x in 0..4

    // The redraw reset runs BEFORE the new outline commits, so the record
    // still holds the OLD resolve — exactly like the strokes.rs call sites.
    c.reset_inherited_seed_for_redraw();
    assert!(c.inherited_seed_pending.contains(&1), "seed re-pended");

    // Re-finalise with a DIFFERENT partial footprint (the right 4 m strip).
    let ring = vec![p(6., 0.), p(10., 0.), p(10., 10.), p(6., 10.)];
    c.document.adopt_finalised_outline(&ring).expect("adopts");
    c.maybe_seed_inherited_levels();

    let below = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(below.ceiling_splits.is_empty(), "still nothing durable");
    // ONLY the pieces under the NEW footprint (x > 6) classify InterStorey —
    // the old x∈[0,4] cover's stamp was cleared by the re-derive.
    let (mut covered_area, mut uncovered_area) = (0.0_f64, 0.0_f64);
    for r in &below.ceilings.regions {
        if r.exposure == CeilingExposure::InterStorey {
            assert!(
                r.centroid().x > 6.0,
                "an InterStorey piece outside the new footprint: {:?}",
                r.centroid()
            );
            covered_area += r.area();
        } else {
            assert!(
                r.centroid().x < 6.0,
                "a piece under the new footprint failed to classify: {:?}",
                r.centroid()
            );
            uncovered_area += r.area();
        }
    }
    assert!(
        (covered_area - 40.0).abs() < 1e-3 && (uncovered_area - 60.0).abs() < 1e-3,
        "classified along x = 6 only: {covered_area} / {uncovered_area}"
    );
}

/// Deleting the top storey clears the captured snap action (and any pending
/// roof area): its `before` snapshot is a whole-`Document` clone with the OLD
/// storey count, so clicking a stale tag after the delete would restore a
/// two-storey Document against the one-storey record.
#[test]
fn delete_storey_clears_last_snap_action() {
    let mut c = dimensioned_square_controller(); // storey 0 = 10x10
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    // Drive a world outline stroke with elective snaps (the
    // `outline_world_stroke_is_captured_for_snap_replay` fixture idiom): the
    // pixel stroke supplies the close-check, the world points get applied.
    c.sketch_start_stroke(Point2f32::new(0.0, 0.0)).unwrap();
    for (x, y) in [(100.0, 0.0), (100.0, 100.0), (0.0, 100.0), (1.0, 1.0)] {
        c.sketch_add_point(Point2f32::new(x, y)).unwrap();
    }
    let world = vec![(0.2, 0.2), (9.8, 0.2), (9.8, 9.8), (0.2, 9.8), (0.3, 0.3)];
    c.apply_outline_stroke_world(&world);
    assert!(
        c.last_snap_action.is_some(),
        "fixture: a snap action was captured"
    );
    assert!(
        !c.snap_overlay().is_empty(),
        "fixture: clickable tags exist"
    );

    c.delete_current_storey().expect("top storey deletes");

    assert!(
        c.last_snap_action.is_none(),
        "the stale snap action is cleared — no replay against the shrunken record"
    );
    assert!(c.snap_overlay().is_empty(), "no stale tags to click");
    assert!(c.pending_roof_area.is_none());
}

// ─── Snap-affordance unification: sub-area carve flows share the tag overlay ─
//
// The six carve-family flows (living area, phase, step, structure, floor +
// ceiling exposure) surface their elective snaps through the SAME snap-tag
// mechanism as outline/extend/trim strokes: the stroke is captured as a
// `SnapAction::Subarea`, tags render from `snap_overlay`, and `choose_snap`
// replays the stroke (plus any recorded cut-region pick) under a forced policy.

/// Fixture: a living-area CARVE (closed loop) whose bottom run hugs the bottom
/// edge up to near corner (0,0) — records a SKIPPABLE `ToCorner` decision (the
/// hug entry at ≈(0.12, 0) snaps to the corner) plus `ToParallelWall` levelings
/// of the near-wall return leg. Commits an area of ≈ 4×3 at the corner.
fn corner_hug_living_area_stroke(c: &mut AppController) {
    c.apply_living_area_stroke_event(&[
        (0.1, 0.05),
        (2.0, 0.05),
        (4.0, 0.05),
        (4.0, 1.5),
        (4.0, 3.0),
        (2.0, 3.0),
        (0.1, 3.0),
        (0.1, 1.5),
        (0.12, 0.1),
    ]);
}

/// A living-area carve that hugs an outline corner surfaces its elective snaps
/// as clickable tags; "Don't snap" (`choose_snap(from, None)`) replays the SAME
/// stroke with that snap skipped — the tag goes grey — and skipping every
/// elective snap reverts the ring to the drawn coordinates.
#[test]
fn living_area_carve_surfaces_snap_tags_and_dont_snap_replays() {
    let mut c = dimensioned_square_controller(); // 10×10, metres
    c.set_living_area_mode(true);
    corner_hug_living_area_stroke(&mut c);
    assert_eq!(c.living_area_count(), 1, "the carve committed an area");
    // The hug entry snapped onto the corner: the ring holds (0,0), not (0.12,0).
    let poly = c.living_area_polygons()[0].clone();
    assert!(
        poly.iter().any(|(x, y)| x.abs() < 1e-3 && y.abs() < 1e-3),
        "hug entry snapped to the corner: {poly:?}"
    );
    let tags = c.snap_overlay();
    let corner = tags
        .iter()
        .find(|t| t.current_kind == Some(manip_plan::SnapKind::ToCorner))
        .expect("the corner hug surfaces as a tag")
        .clone();
    assert!(corner.skippable, "a hug-to-corner is elective — skippable");

    // "Don't snap" on the corner tag: the action replays and the tag goes grey.
    c.choose_snap(Point2f32::new(corner.from.0, corner.from.1), None);
    assert_eq!(c.living_area_count(), 1, "replay re-committed the area");
    let grey = c
        .snap_overlay()
        .into_iter()
        .find(|t| {
            (t.from.0 - corner.from.0).abs() < 1e-3 && (t.from.1 - corner.from.1).abs() < 1e-3
        })
        .expect("the corner tag survives the replay");
    assert_eq!(grey.current_kind, None, "the corner tag went grey");

    // Skip every remaining elective snap, one tag at a time (each pass through
    // the same choose_snap → replay → re-capture loop the outline tools use).
    let mut guard = 0;
    while let Some(tag) = c
        .snap_overlay()
        .into_iter()
        .find(|t| t.skippable && t.current_kind.is_some())
    {
        assert!(guard < 8, "un-snap loop failed to converge");
        c.choose_snap(Point2f32::new(tag.from.0, tag.from.1), None);
        guard += 1;
    }
    assert_eq!(c.living_area_count(), 1);
    // Every snap skipped → the ring reverts to the DRAWN coordinates: the hug
    // entry at x≈0.12 (not the corner) and the return-leg nook at ≈(0, 0.2).
    let poly = c.living_area_polygons()[0].clone();
    assert!(
        poly.iter()
            .any(|(x, y)| (x - 0.12).abs() < 0.02 && y.abs() < 1e-3),
        "un-snapped: the drawn hug entry is back: {poly:?}"
    );
    assert!(
        !poly.iter().any(|(x, y)| x.abs() < 1e-3 && y.abs() < 1e-3),
        "un-snapped: the corner vertex is gone: {poly:?}"
    );
    assert!(
        c.snap_overlay()
            .iter()
            .all(|t| t.current_kind.is_none() || !t.skippable),
        "every elective tag shows un-snapped"
    );
}

/// A living-area straight (stepped) CUT records its MANDATORY endpoint attach
/// (skippable = false — the UI omits "Don't snap" for it) and the tags are
/// clickable while the region pick is still pending; picking a region commits
/// the area and keeps the tags alive with the pick recorded, so choosing the
/// alternative attach (corner vs edge) replays stroke + pick in one go.
#[test]
fn living_area_cut_tags_survive_the_region_pick_and_replay_reapplies_it() {
    let mut c = dimensioned_square_controller(); // 10×10, metres
    c.set_living_area_mode(true);
    // Stepped cut: bottom edge up to y=5, right to x=9.7, up to near corner
    // (10,10) — the end within vertex reach of the corner, with the top edge as
    // the perpendicular (ToEdge) alternative.
    c.apply_living_area_stroke_event(&[(5.0, 0.2), (5.0, 5.0), (9.7, 5.0), (9.7, 9.75)]);
    assert!(c.cut_pick_pending(), "the cut awaits its region pick");
    assert_eq!(c.living_area_count(), 0, "nothing committed until picked");
    // The mandatory endpoint attach is tagged WHILE the pick is pending.
    let tag = c
        .snap_overlay()
        .into_iter()
        .find(|t| !t.skippable)
        .expect("the mandatory attach surfaces as a tag before the pick");
    assert_eq!(tag.current_kind, Some(manip_plan::SnapKind::ToVertex));
    let kinds: Vec<_> = tag.options.iter().map(|(k, _)| *k).collect();
    assert!(
        kinds.contains(&manip_plan::SnapKind::ToVertex)
            && kinds.contains(&manip_plan::SnapKind::ToEdge),
        "corner AND edge alternatives offered: {kinds:?}"
    );

    // Pick the bottom-right region: commits the area, tags stay up, and the
    // pick is recorded on the captured action for replay.
    c.choose_cut_region(7.0, 2.0);
    assert_eq!(c.living_area_count(), 1, "the picked region committed");
    assert!(!c.cut_pick_pending());
    assert!(!c.snap_overlay().is_empty(), "tags survive the pick");
    match c.last_snap_action.as_ref().map(|l| &l.action) {
        Some(SnapAction::Subarea { pick, .. }) => {
            assert_eq!(*pick, Some((7.0, 2.0)), "the pick is recorded for replay")
        }
        other => panic!("expected a Subarea capture, got {other:?}"),
    }

    // Choose the ALTERNATIVE attach (edge instead of corner): the replay re-runs
    // the stroke under the forced choice AND re-applies the pick.
    let hlen_before = c.history.len();
    let edge_idx = tag
        .options
        .iter()
        .find(|(k, _)| *k == manip_plan::SnapKind::ToEdge)
        .unwrap()
        .1;
    c.choose_snap(Point2f32::new(tag.from.0, tag.from.1), Some(edge_idx));
    assert_eq!(
        c.living_area_count(),
        1,
        "exactly one living area after replay (the first commit was rewound)"
    );
    assert!(!c.cut_pick_pending(), "the pick was re-applied");
    let retagged = c
        .snap_overlay()
        .into_iter()
        .find(|t| !t.skippable)
        .expect("the mandatory tag re-captured");
    assert_eq!(
        retagged.current_kind,
        Some(manip_plan::SnapKind::ToEdge),
        "the forced edge attach is now the applied kind"
    );
    assert_eq!(
        c.history.len(),
        hlen_before,
        "replay rewound the old commit and pushed exactly the re-run's entry"
    );
    // Undo unwinds the replayed commit cleanly.
    c.undo().unwrap();
    assert_eq!(c.living_area_count(), 0, "undo removes the replayed area");
}

/// Cancelling a pending cut (tapping outside both regions) clears the snap
/// tags — the geometry they describe no longer exists.
#[test]
fn cancelling_a_pending_cut_clears_the_snap_tags() {
    let mut c = dimensioned_square_controller();
    c.set_living_area_mode(true);
    c.apply_living_area_stroke_event(&[(5.0, 0.2), (5.0, 5.0), (9.7, 5.0), (9.7, 9.75)]);
    assert!(c.cut_pick_pending());
    assert!(!c.snap_overlay().is_empty(), "tags up while pick pending");
    c.choose_cut_region(50.0, 50.0); // outside both → cancel
    assert!(!c.cut_pick_pending());
    assert_eq!(c.living_area_count(), 0);
    assert!(
        c.snap_overlay().is_empty(),
        "cancel dropped the capture — no stale tags"
    );
}

/// Leak regression: a living-area carve's decisions are drained into ITS OWN
/// capture, so a later extend stroke's capture holds ONLY the extend's
/// decisions (the old silent flows left theirs in the thread-local, where the
/// next tier-1 capture would swallow them).
#[test]
fn subarea_decisions_do_not_leak_into_a_later_extend_capture() {
    let mut c = dimensioned_square_controller();
    c.set_living_area_mode(true);
    corner_hug_living_area_stroke(&mut c); // snaps near y≈0 (bottom-left corner)
    assert!(
        !c.snap_overlay().is_empty(),
        "the carve captured its own decisions"
    );
    // Leave the area un-touched (no tag interaction) and extend the OUTLINE:
    // a porch off the top edge whose left attach lands near corner (0,10) —
    // a mandatory corner-vs-edge attach decision at y≈10.
    c.set_living_area_mode(false);
    c.set_edit_mode(EditTools::ExtendOrtho);
    c.apply_extend_stroke_event(&[(0.3, 9.8), (0.3, 12.0), (4.0, 12.0), (4.0, 9.9)]);
    let last = c.last_snap_action.as_ref().expect("the extend captured");
    assert!(
        matches!(last.action, SnapAction::Freehand { .. }),
        "the capture is the extend stroke"
    );
    assert!(
        !last.decisions.is_empty(),
        "the extend's own attach decision was recorded"
    );
    assert!(
        last.decisions.iter().all(|d| d.from.y > 5.0),
        "ONLY the extend's decisions (top edge) — nothing leaked from the \
         living-area carve (bottom corner): {:?}",
        last.decisions.iter().map(|d| d.from).collect::<Vec<_>>()
    );
}

/// A later action that makes NO elective snaps still supersedes the previous
/// action's capture: the old tags must NOT survive it — replaying them would
/// truncate/restore history from before the new action and silently wipe its
/// work.
#[test]
fn a_non_snapping_action_clears_the_previous_actions_tags() {
    let mut c = dimensioned_square_controller(); // 10×10, metres
    c.set_living_area_mode(true);
    corner_hug_living_area_stroke(&mut c);
    assert!(!c.snap_overlay().is_empty(), "carve A's tags are up");
    // Carve B: a free-standing interior room, well clear of every wall — no
    // elective snap fires, but it commits (and pushes history entries A's
    // stale capture knows nothing about).
    c.apply_living_area_stroke_event(&[(6.0, 6.0), (8.0, 6.0), (8.0, 8.0), (6.0, 8.0), (6.0, 6.0)]);
    assert_eq!(c.living_area_count(), 2, "carve B committed");
    assert!(
        c.snap_overlay().is_empty(),
        "carve A's stale tags are gone — nothing left to mis-replay"
    );
}

/// Undo invalidates the snap capture: its `hlen` indexes into the history
/// stack undo just popped, so a replay would restore the very state the undo
/// removed. The tags disappear with the undone action.
#[test]
fn undo_clears_the_snap_capture() {
    let mut c = dimensioned_square_controller();
    c.set_living_area_mode(true);
    corner_hug_living_area_stroke(&mut c);
    assert!(!c.snap_overlay().is_empty(), "the carve's tags are up");
    c.undo().unwrap();
    assert_eq!(c.living_area_count(), 0, "the carve was undone");
    assert!(
        c.snap_overlay().is_empty(),
        "the undone action's tags are gone"
    );
}

/// Smoke: the phase-region Area path surfaces its carve snaps as tags.
#[test]
fn phase_region_carve_surfaces_snap_tags() {
    let mut c = dimensioned_square_controller();
    c.apply_phase_region_stroke_event(&[
        (0.1, 0.05),
        (2.0, 0.05),
        (4.0, 0.05),
        (4.0, 1.5),
        (4.0, 3.0),
        (2.0, 3.0),
        (0.1, 3.0),
        (0.1, 1.5),
        (0.12, 0.1),
    ]);
    assert_eq!(
        c.document.active,
        ActiveShape::PhaseArea,
        "the carved phase area is dimensioning"
    );
    assert!(
        !c.snap_overlay().is_empty(),
        "the phase carve's elective snaps surface as tags"
    );
}

/// Smoke: the step-region Area path surfaces its carve snaps as tags.
#[test]
fn step_region_carve_surfaces_snap_tags() {
    let mut c = dimensioned_square_controller();
    c.apply_step_region_stroke_event(&[
        (0.1, 0.05),
        (2.0, 0.05),
        (4.0, 0.05),
        (4.0, 1.5),
        (4.0, 3.0),
        (2.0, 3.0),
        (0.1, 3.0),
        (0.1, 1.5),
        (0.12, 0.1),
    ]);
    assert_eq!(
        c.document.active,
        ActiveShape::StepArea,
        "the carved step area is dimensioning"
    );
    assert!(
        !c.snap_overlay().is_empty(),
        "the step carve's elective snaps surface as tags"
    );
}

/// The ceiling-step AREA route (a closed/multi-side stroke under ceiling-step
/// mode — NOT the straight-line route, which keeps its own bespoke tag)
/// surfaces its carve snaps as tags like every other carve flow; "Don't snap"
/// replays the stroke with the snap skipped, and leaving the mode (which
/// abandons the in-flight carve) drops the tags.
#[test]
fn ceiling_step_carve_surfaces_snap_tags_and_mode_exit_clears_them() {
    let mut c = ceiling_ready_controller(); // 10×10, height set, mode on
                                            // The corner-hug loop (same shape as the living-area fixture) through the
                                            // CEILING entry: not a straight line → routed to the AREA carve.
    c.apply_ceiling_step_stroke_event(&[
        (0.1, 0.05),
        (2.0, 0.05),
        (4.0, 0.05),
        (4.0, 1.5),
        (4.0, 3.0),
        (2.0, 3.0),
        (0.1, 3.0),
        (0.1, 1.5),
        (0.12, 0.1),
    ]);
    assert_eq!(
        c.document.active,
        ActiveShape::StepArea,
        "the carved ceiling area is dimensioning"
    );
    let corner = c
        .snap_overlay()
        .into_iter()
        .find(|t| t.current_kind == Some(manip_plan::SnapKind::ToCorner))
        .expect("the corner hug surfaces as a tag");
    assert!(corner.skippable, "a hug-to-corner is elective");
    // The area ring holds the snapped corner (0,0).
    let ring = c.document.floor_plan.get_points();
    assert!(
        ring.iter().any(|p| p.x.abs() < 1e-3 && p.y.abs() < 1e-3),
        "hug entry snapped to the corner: {ring:?}"
    );

    // "Don't snap": the stroke replays through the ceiling entry (the mode is
    // still on) and the corner reverts to the drawn coordinate.
    c.choose_snap(Point2f32::new(corner.from.0, corner.from.1), None);
    assert_eq!(
        c.document.active,
        ActiveShape::StepArea,
        "the replay re-carved the ceiling area"
    );
    let grey = c
        .snap_overlay()
        .into_iter()
        .find(|t| {
            (t.from.0 - corner.from.0).abs() < 1e-3 && (t.from.1 - corner.from.1).abs() < 1e-3
        })
        .expect("the corner tag survives the replay");
    assert_eq!(grey.current_kind, None, "the corner tag went grey");

    // Leaving the mode abandons the carve — its tags must go with it.
    c.set_ceiling_step_mode(false);
    assert!(
        c.snap_overlay().is_empty(),
        "mode exit dropped the abandoned carve's tags"
    );
}

/// History-truncate regression: entries pushed AFTER the captured stroke (a
/// dimension answer) are rewound by the replay too — the old `history.pop()`
/// removed only the newest entry (the dimension's), leaving a stale entry and
/// corrupting the undo chain.
#[test]
fn replay_after_a_dimension_answer_rewinds_history_to_pre_stroke() {
    let mut c = dimensioned_square_controller();
    c.set_living_area_mode(true);
    let h0 = c.history.len();
    corner_hug_living_area_stroke(&mut c);
    assert_eq!(c.living_area_count(), 1);
    assert_eq!(c.history.len(), h0 + 1, "the carve pushed its undo entry");
    // Answer a dimension prompt for one of the drawn walls — pushes another.
    assert!(
        c.document.dimensioning.current_prompt.is_some(),
        "the drawn walls prompt for dimensions"
    );
    c.apply_dimension(3.0).expect("dimension applies");
    assert_eq!(c.history.len(), h0 + 2, "the answer pushed its own entry");

    // "Don't snap" on the corner tag: the replay must rewind PAST the dimension
    // answer to the pre-stroke state, then re-run the stroke.
    let corner = c
        .snap_overlay()
        .into_iter()
        .find(|t| t.current_kind == Some(manip_plan::SnapKind::ToCorner))
        .expect("corner tag present");
    c.choose_snap(Point2f32::new(corner.from.0, corner.from.1), None);
    assert_eq!(c.living_area_count(), 1, "the re-run re-committed the area");
    assert_eq!(
        c.history.len(),
        h0 + 1,
        "history rewound to pre-stroke plus the re-run's single entry"
    );
    // The undo chain is intact: one undo removes the (replayed) area entirely.
    c.undo().unwrap();
    assert_eq!(
        c.living_area_count(),
        0,
        "undo lands at the pre-stroke state"
    );
    assert_eq!(c.history.len(), h0);
}

// ─── Wall-face partition: snap tags + "Don't snap" (Phase B) ────────────────

/// Helper: open the partition view on wall 0 of the 10×10 outline (10 m long,
/// default 2.4 m face height); returns the wall's vid.
fn open_partition_on_wall0(c: &mut AppController) -> VertexId {
    let walls = c.document.outline.floor_plan.perimeter_walls();
    let (vid, a, b) = walls[0];
    let mid = ((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
    c.set_exposure_mode(true);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.open_wall_partition();
    assert!(c.partition_mode());
    assert_eq!(c.partition_wall(), Some(vid));
    vid
}

/// A cut whose A endpoint silently snaps in x (drawn near along=0) exposes ONE
/// "edge" tag; "Don't snap" reverts a.x to the drawn value and REOPENS its
/// "distance along" prompt at the head of the queue; typing a value then
/// commits the cut with the typed coordinate.
#[test]
fn partition_unsnap_reverts_the_endpoint_and_reopens_its_prompt() {
    let mut c = dimensioned_square_controller();
    let vid = open_partition_on_wall0(&mut c);
    // A drawn at (0.05, 1.0): x snaps to the start edge (eps 0.15 m), y free.
    // B drawn at (4.0, 1.8): both free. Genuinely diagonal → per-endpoint slots.
    c.partition_stroke_point(0.05, 1.0);
    c.partition_stroke_point(2.0, 1.4);
    c.partition_stroke_point(4.0, 1.8);
    c.partition_stroke_end();
    let view = c.partition_view().unwrap();
    assert_eq!(view.snap_tags.len(), 1, "one snapped endpoint → one tag");
    let tag = view.snap_tags[0];
    assert!(!tag.end_b, "the snapped endpoint is A");
    assert!(!tag.corner, "one snapped axis reads as an edge tag");
    assert!(
        (tag.pos.0 - 0.0).abs() < 1e-6 && (tag.pos.1 - 1.0).abs() < 1e-6,
        "tag at the snapped position"
    );
    // a.x was resolved by the snap, so the queue head is a.y.
    assert_eq!(
        c.partition_dim_prompt().unwrap().label,
        "Height from floor (m)"
    );

    // Un-snapping end B (nothing snapped there) changes nothing.
    c.unsnap_partition_end(true);
    assert_eq!(
        c.partition_dim_prompt().unwrap().label,
        "Height from floor (m)"
    );

    c.unsnap_partition_end(false);
    let p = c.partition_pending.as_ref().expect("still pending");
    assert!(
        (p.a.x - 0.05).abs() < 1e-6,
        "a.x reverted to the drawn value"
    );
    // The freed a.x rejoins the derived queue at its canonical head slot.
    assert_eq!(
        c.partition_dim_prompt().unwrap().label,
        "Distance along (m)"
    );
    assert!(
        c.partition_view().unwrap().snap_tags.is_empty(),
        "no snapped coordinates left → no tags"
    );

    // Dimension all four coords; the typed a.x lands in the committed cut.
    c.partition_dim_entry(0.3); // a.x
    c.partition_dim_entry(1.0); // a.y
    c.partition_dim_entry(4.0); // b.x
    c.partition_dim_entry(1.8); // b.y
    assert!(c.partition_pending.is_none(), "queue emptied → commit");
    let part = c.document.wall_face_partitions.get(&vid).unwrap();
    assert_eq!(part.cuts.len(), 1);
    assert!(
        (part.cuts[0].a.x - 0.3).abs() < 1e-3,
        "the typed (un-snapped) coordinate committed"
    );
}

/// Regression on the derived queue: a near-vertical cut with both x's free
/// still collapses to ONE shared "distance along" slot, and one entry commits
/// both endpoints — the queue derivation must not split the shared slot.
#[test]
fn partition_derived_queue_keeps_the_shared_along_slot() {
    let mut c = dimensioned_square_controller();
    let vid = open_partition_on_wall0(&mut c);
    // Vertical drag mid-wall: y snaps at both ends (floor/ceiling), the two
    // near-equal free x's collapse to one shared slot.
    c.partition_stroke_point(5.0, 0.0);
    c.partition_stroke_point(5.02, 1.2);
    c.partition_stroke_point(5.0, 2.4);
    c.partition_stroke_end();
    let prompt = c.partition_dim_prompt().expect("a free slot to prompt");
    assert_eq!(prompt.label, "Distance from start corner (m)");
    // Both endpoints y-snapped → two edge tags.
    let view = c.partition_view().unwrap();
    assert_eq!(view.snap_tags.len(), 2);
    assert!(view.snap_tags.iter().all(|t| !t.corner));
    // ONE entry resolves both x's and commits.
    c.partition_dim_entry(5.0);
    assert!(c.partition_pending.is_none(), "one shared entry → commit");
    let part = c.document.wall_face_partitions.get(&vid).unwrap();
    assert_eq!(part.cuts.len(), 1);
    assert!((part.cuts[0].a.x - 5.0).abs() < 1e-3 && (part.cuts[0].b.x - 5.0).abs() < 1e-3);
}

/// A fully-snapped stroke commits INSTANTLY but its tags stay visible, and
/// "Don't snap" UN-COMMITS it: history rewinds past the commit, the cut
/// reopens as pending with the tapped end's axes freed at the drawn values,
/// and dimensioning then completes and commits again.
#[test]
fn partition_instant_commit_tags_allow_uncommit_and_redimension() {
    let mut c = dimensioned_square_controller();
    let vid = open_partition_on_wall0(&mut c);
    // Both drawn endpoints within eps of the start-edge corners → every
    // coordinate snaps → the cut commits with no interaction at all.
    c.partition_stroke_point(0.05, 0.05);
    c.partition_stroke_point(0.02, 1.2);
    c.partition_stroke_point(0.03, 2.35);
    c.partition_stroke_end();
    assert!(
        c.partition_pending.is_none(),
        "fully snapped → instant commit"
    );
    assert!(c.document.is_wall_partitioned(vid));
    let hlen_after = c.history.len();
    // The tags still show for the just-committed cut.
    let view = c.partition_view().unwrap();
    assert_eq!(view.snap_tags.len(), 2, "both endpoints tagged");
    assert!(
        view.snap_tags.iter().all(|t| t.corner),
        "both axes snapped → corner tags"
    );

    // "Don't snap" on end B un-commits: the commit's undo entry is popped +
    // restored, and the cut reopens as pending with b's axes freed.
    c.unsnap_partition_end(true);
    assert!(
        !c.document.is_wall_partitioned(vid),
        "the instant commit rewound"
    );
    assert_eq!(
        c.history.len(),
        hlen_after - 1,
        "the commit's undo entry was dropped"
    );
    let p = c
        .partition_pending
        .as_ref()
        .expect("cut reopened as pending");
    assert!(
        (p.b.x - 0.03).abs() < 1e-6 && (p.b.y - 2.35).abs() < 1e-6,
        "b back at the drawn point"
    );
    assert_eq!(
        c.partition_dim_prompt().unwrap().label,
        "Distance along (m)"
    );

    // Dimensioning completes → the cut commits again with the typed b.
    c.partition_dim_entry(3.0); // b.x
    c.partition_dim_entry(2.4); // b.y
    assert!(c.partition_pending.is_none(), "queue emptied → re-commit");
    let part = c.document.wall_face_partitions.get(&vid).unwrap();
    assert_eq!(part.cuts.len(), 1);
    assert!((part.cuts[0].b.x - 3.0).abs() < 1e-3);
}

/// After an unrelated mutation the instant-commit record is STALE: its tags
/// disappear and "Don't snap" is a no-op (no rewind, no pending cut).
#[test]
fn partition_instant_commit_record_goes_stale_after_another_mutation() {
    let mut c = dimensioned_square_controller();
    let vid = open_partition_on_wall0(&mut c);
    c.partition_stroke_point(0.05, 0.05);
    c.partition_stroke_point(0.02, 1.2);
    c.partition_stroke_point(0.03, 2.35);
    c.partition_stroke_end();
    assert!(c.document.is_wall_partitioned(vid));
    assert_eq!(c.partition_view().unwrap().snap_tags.len(), 2);

    // An unrelated mutation (attributing a region pushes its own undo entry).
    c.select_partition_region(5.0, 1.2);
    c.set_partition_region_exposure(WallExposure::Exposed);
    let hlen = c.history.len();

    assert!(
        c.partition_view().unwrap().snap_tags.is_empty(),
        "stale record exposes no tags"
    );
    c.unsnap_partition_end(true);
    assert!(c.partition_pending.is_none(), "stale un-snap is a no-op");
    assert!(
        c.document.is_wall_partitioned(vid),
        "the cut stays committed"
    );
    assert_eq!(c.history.len(), hlen, "history untouched");
}

/// `unsnap_partition_end` with nothing pending and nothing committed is a
/// clean no-op.
#[test]
fn partition_unsnap_with_nothing_pending_is_a_noop() {
    let mut c = dimensioned_square_controller();
    let vid = open_partition_on_wall0(&mut c);
    let hlen = c.history.len();
    c.unsnap_partition_end(false);
    c.unsnap_partition_end(true);
    assert!(c.partition_pending.is_none());
    assert!(!c.document.is_wall_partitioned(vid));
    assert_eq!(c.history.len(), hlen);
    assert!(c.partition_view().unwrap().snap_tags.is_empty());
}

// ── InterStorey derivation: the riser fixture through the REAL pipeline ─────

/// Walls of `storey` whose base edge lies on the plan line x = `x` (both
/// endpoints), with their z extent — for asserting the step-line splits.
fn walls_on_x_with_extent(
    storey: &ber_model::ber::structural::Storey,
    x: f64,
) -> Vec<(&ber_model::ber::structural::WallRegion, f64, f64)> {
    storey
        .walls
        .regions
        .iter()
        .filter(|w| {
            w.base_edge()
                .is_some_and(|(a, b)| (a.x - x).abs() < 1e-6 && (b.x - x).abs() < 1e-6)
        })
        .map(|w| {
            let (lo, hi) = w
                .polygon
                .exterior
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(v.z), hi.max(v.z))
                });
            (w, lo, hi)
        })
        .collect()
}

/// THE USER'S RISER FIXTURE through the REAL controller pipeline (spec §6
/// phase 3): ground storey 10×10 with ceiling 2.4 m, a ceiling STEP at x = 4
/// raising the right section to 4.0 m (delta +1.6 — a riser wall on the step
/// line), first floor (slab 0.3 m) adopted over the LOW section x∈[0,4], its
/// walls painted Exposed. All via controller ops — stroke-fitted step line,
/// side tap, position + delta prompts, add-storey flow, finalised adoption,
/// bulk paint. Datums: ground 0, first floor 2.4 + 0.3 = 2.7.
fn inter_storey_riser_fixture_controller() -> AppController {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let mut c = ceiling_ready_controller(); // 10×10, height 2.4, step mode on
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(1.6).expect("delta applies"); // right at 4.0 m
    c.set_ceiling_step_mode(false);
    c.materialize();

    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    // First floor over the LOW section (left strip x∈[0,4]).
    let ring = vec![p(0., 0.), p(4., 0.), p(4., 10.), p(0., 10.)];
    c.document.adopt_finalised_outline(&ring).expect("adopts");
    c.maybe_seed_inherited_levels();
    c.set_default_ceiling_height(2.4).expect("height applies");
    // Paint every first-floor wall Exposed (the remainder above the buried
    // band must KEEP this prior exposure through the split).
    c.set_all_remaining_exposed();
    c.materialize();
    c
}

/// End-to-end riser fixture (spec §6 phase 3, test 1): the storey-1 wall on
/// the step line x = 4 splits into a lower InterStorey band (buried behind the
/// ground storey's TALL section — top at 4.0 abs = 1.3 storey-local) and an
/// Exposed remainder; the ground storey's LOW ceiling section under storey 1
/// derives InterStorey (the tall one doesn't); the ground step riser is fully
/// buried in storey 1's backing band and stamps whole.
#[test]
fn riser_fixture_derives_inter_storey_through_the_real_pipeline() {
    use ber_model::ber::structural::{CeilingExposure, FloorExposure, WallExposure};
    let c = inter_storey_riser_fixture_controller();
    let storeys = &c.current_record.as_ref().unwrap().data.storeys;

    // Storey-1 wall on the step line: split into the buried band + remainder.
    let step_walls = walls_on_x_with_extent(&storeys[1], 4.0);
    assert_eq!(
        step_walls.len(),
        2,
        "the storey-1 step-line wall split into two bands: {:?}",
        step_walls
            .iter()
            .map(|(w, lo, hi)| (w.exposure, *lo, *hi))
            .collect::<Vec<_>>()
    );
    let (lower, upper) = if step_walls[0].2 <= step_walls[1].1 + 1e-4 {
        (&step_walls[0], &step_walls[1])
    } else {
        (&step_walls[1], &step_walls[0])
    };
    // The wall's own base sits at the slab underside (-0.3 storey-local = the
    // ground ceiling 2.4 abs); the band top is the TALL ground-roof z
    // (4.0 abs = 1.3 storey-local).
    assert!(
        (lower.1 + 0.3).abs() < 1e-4 && (lower.2 - 1.3).abs() < 1e-4,
        "buried band spans z -0.3..1.3 (top at the tall ground roof), got {}..{}",
        lower.1,
        lower.2
    );
    assert_eq!(
        lower.0.exposure,
        WallExposure::InterStorey,
        "the buried band derives InterStorey"
    );
    assert!(
        (upper.1 - 1.3).abs() < 1e-4 && (upper.2 - 2.4).abs() < 1e-4,
        "remainder spans z 1.3..2.4, got {}..{}",
        upper.1,
        upper.2
    );
    assert_eq!(
        upper.0.exposure,
        WallExposure::Exposed,
        "the remainder keeps its painted exposure"
    );

    // Ground storey: the LOW ceiling section (flat 2.4, under storey 1) is
    // InterStorey; the TALL section (4.0) stays labellable.
    for r in &storeys[0].ceilings.regions {
        match r.flat_z() {
            Some(z) if (z - 2.4).abs() < 1e-6 => {
                assert_eq!(r.exposure, CeilingExposure::InterStorey)
            }
            Some(z) if (z - 4.0).abs() < 1e-6 => {
                assert_ne!(r.exposure, CeilingExposure::InterStorey)
            }
            other => panic!("unexpected ceiling z {other:?}"),
        }
    }
    // The ground step riser (z 2.4..4.0 on x=4) is fully inside storey 1's
    // backing band [2.4, 5.1] → stamped whole.
    let risers = walls_on_x_with_extent(&storeys[0], 4.0);
    assert_eq!(risers.len(), 1, "the ground step riser stays whole");
    assert_eq!(risers[0].0.exposure, WallExposure::InterStorey);
    // Storey 1's floor sits fully over the ground storey → InterStorey.
    assert!(storeys[1]
        .floors
        .regions
        .iter()
        .all(|r| r.exposure == FloorExposure::InterStorey));
}

/// Save → reload → materialize FIXPOINT over the riser fixture (spec §6
/// phase 3, test 2), to the standard of
/// `flush_riser_merge_survives_reload_and_rederive_fixpoint`: the full
/// region set — geometry AND exposures, every kind, every storey — is stable
/// across persist → `open_ber` → re-derive.
#[test]
fn inter_storey_riser_fixture_reload_and_rederive_fixpoint() {
    let mut c = inter_storey_riser_fixture_controller();
    let fingerprint = |c: &AppController| -> String {
        c.current_record
            .as_ref()
            .unwrap()
            .data
            .storeys
            .iter()
            .map(|s| {
                let mut out = String::new();
                for r in &s.walls.regions {
                    out += &format!("W {:?} {:?};", r.exposure, r.polygon);
                }
                for r in &s.floors.regions {
                    out += &format!("F {:?} {:?};", r.exposure, r.polygon);
                }
                for r in &s.ceilings.regions {
                    out += &format!("C {:?} {:?};", r.exposure, r.polygon);
                }
                out
            })
            .collect::<Vec<_>>()
            .join("|")
    };
    let before = fingerprint(&c);
    assert!(before.contains("InterStorey"), "fixture derived something");

    let id = c.current_record.as_ref().unwrap().id;
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    assert_eq!(
        fingerprint(&c),
        before,
        "reload + first re-derive is a fixpoint"
    );

    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        fingerprint(&c),
        before,
        "a further no-edit re-derive stays at the fixpoint"
    );
}

/// LEGACY-RECORD MIGRATION (spec §6 phase 3, test 3): a record persisted by
/// the OLD auto-party mechanism — the covered ceiling piece stamped `Party`,
/// a party carve ring on the below storey's `ceiling_splits`, and the Party
/// auto-seed in the exposure map — loads to the NEW classification: the
/// covered piece resolves `InterStorey`, the already-carved face is NOT
/// re-split (no duplicates), and the load-time purge drops the shadowed map
/// entry. The legacy ring itself is consumed as-is (it stays until a
/// delete/redraw retracts it).
#[test]
fn legacy_party_record_migrates_to_inter_storey_on_load() {
    use ber_model::ber::structural::CeilingExposure;
    let mut c = partial_cover_two_storey_controller(); // covered piece x∈[0,4]

    // Rebuild the OLD-style persisted state directly on the record: the party
    // carve ring under the upper footprint, the covered piece stamped Party,
    // and the legacy auto-seeded Party entry in the doc map.
    let ring: Vec<Point2f64> = vec![
        Point2f64::new(0.0, 0.0),
        Point2f64::new(4.0, 0.0),
        Point2f64::new(4.0, 10.0),
        Point2f64::new(0.0, 10.0),
    ];
    {
        let below = &mut c.current_record.as_mut().unwrap().data.storeys[0];
        below.ceiling_splits.push(ring);
        let covered = below
            .ceilings
            .regions
            .iter_mut()
            .find(|r| r.centroid().x < 4.0)
            .expect("the covered piece");
        covered.exposure = CeilingExposure::Party;
        let key = document::centroid_key(covered.centroid());
        c.document.storeys[0]
            .ceiling_exposure
            .insert(key, CeilingExposure::Party);
    }
    // Persist the legacy state VERBATIM (no materialize — a flush would
    // re-derive and overwrite the old-style stamps before they hit disk).
    c.persist().expect("legacy record persists");
    let id = c.current_record.as_ref().unwrap().id;
    c.saved_revision = c.revision; // open_ber's boundary flush must not re-derive

    c.open_ber(&id).unwrap();
    let below = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(
        below.ceilings.regions.len(),
        2,
        "no duplicate split — the already-carved face is intact"
    );
    let covered = below
        .ceilings
        .regions
        .iter()
        .find(|r| r.centroid().x < 4.0)
        .expect("the covered piece survives");
    assert_eq!(
        covered.exposure,
        CeilingExposure::InterStorey,
        "the old auto-Party piece resolves InterStorey"
    );
    assert!(
        !c.document.storeys[0]
            .ceiling_exposure
            .values()
            .any(|e| *e == CeilingExposure::Party),
        "the shadowed legacy Party map entry was purged"
    );
    assert_eq!(
        below.ceiling_splits.len(),
        1,
        "the legacy ring is consumed as-is (kept until delete/redraw)"
    );
    // Stability: a further re-derive neither duplicates nor reclassifies.
    c.mark_dirty().unwrap();
    c.materialize();
    let below = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(below.ceilings.regions.len(), 2);
    assert!(below
        .ceilings
        .regions
        .iter()
        .any(|r| r.exposure == CeilingExposure::InterStorey));
}

/// GATE TEST (spec §6 phase 3, test 4): the "Wall exposures" checklist row
/// goes green WITHOUT the assessor ever touching the buried band — paint
/// everything else (both storeys' bulk pass), and the derived InterStorey
/// pieces (the storey-1 band, the ground step riser) count as classified.
#[test]
fn wall_checklist_goes_green_without_painting_the_buried_band() {
    use ber_model::ber::structural::WallExposure;
    // The fixture already bulk-painted storey 1 (active); paint storey 0 too.
    let mut c = inter_storey_riser_fixture_controller();
    c.set_current_storey(Some(0)).expect("switch to ground");
    c.set_all_remaining_exposed();
    c.materialize();

    // The derived pieces were never painted — still InterStorey.
    let storeys = &c.current_record.as_ref().unwrap().data.storeys;
    assert!(
        storeys[1]
            .walls
            .regions
            .iter()
            .any(|r| r.exposure == WallExposure::InterStorey),
        "the buried band is untouched (nothing painted it)"
    );
    assert!(
        storeys[0]
            .walls
            .regions
            .iter()
            .any(|r| r.exposure == WallExposure::InterStorey),
        "the ground step riser is untouched (the bulk pass skips it)"
    );
    assert!(
        storeys.iter().all(|s| s
            .walls
            .regions
            .iter()
            .all(|r| r.exposure != WallExposure::Unset)),
        "no Unset wall piece remains anywhere"
    );
    assert_eq!(
        checklist_status_of(&c, "Wall exposures"),
        ChecklistStatus::AllStoreys,
        "the wall checklist is green without painting the InterStorey pieces"
    );
}

/// OPENINGS ON A DERIVATION-SPLIT WALL (review High + user verdict): a window
/// placed on the riser fixture's step wall — its rectangle in the EXPOSED
/// remainder above the buried band — stays fully linked through every
/// materialize and a reload, and is ALWAYS hosted by / counted on the
/// labellable remainder, never the InterStorey band. The remainder is a
/// first-class wall now (`rebase_ring_start` gives the horizontal split's
/// upper piece a real base frame at the cut height); the pipeline's
/// post-derive re-link re-homes the placement z-aware
/// (`host_wall_for_rect`), rebasing `sill_m` onto each host's `base_z` so
/// the world rectangle — and the placement fields at rest — are invariant.
/// Without the re-link: placement.wall_id matches no live region (dead
/// opening drag), every piece reads empty `opes`, and an anchor-less
/// placement is pruned on reload.
#[test]
fn window_on_derivation_split_wall_stays_linked_and_survives_reload() {
    use ber_model::ber::fenestration::{Frame, Glazing, OpeningPlacement};
    use ber_model::ber::structural::WallExposure;
    let mut c = inter_storey_riser_fixture_controller();

    // Host = the EXPOSED remainder piece (what `pick_wall_along` returns for a
    // tap on this wall — it skips the buried band). Its base sits at the cut
    // height (~1.3 local); a sill of (1.5 − base_z) puts the 0.5 m window at
    // z 1.5..2.0, fully inside the remainder band (1.3..2.4).
    let (host_id, sill) = {
        let storeys = &c.current_record.as_ref().unwrap().data.storeys;
        let (host, _, _) = walls_on_x_with_extent(&storeys[1], 4.0)
            .into_iter()
            .find(|(w, _, _)| w.exposure != WallExposure::InterStorey)
            .expect("the exposed remainder piece");
        let base_z = host.base_frame().expect("first-class host").3;
        (host.id, (1.5 - base_z) as f32)
    };
    let type_id = c.current_record.as_mut().unwrap().data.ensure_window_type(
        Glazing::Double,
        Frame::WoodUPVC,
        0,
        false,
        Vec::new(),
        None,
    );
    let win_id = ber_model::ber::openings::add_wall_window(
        &mut c.current_record.as_mut().unwrap().data.storeys[1],
        host_id,
        5.0,
        1.0,
        0.5,
        sill,
        type_id,
    )
    .expect("window placed on the step wall");
    // `add_wall_window`'s vertical-stacking fallback picks its own sill — pin
    // it back, the state the assessor's sill edit leaves in the record.
    {
        use ber_model::ber::fenestration::OpeningPlacement as OP;
        let w = &mut c.current_record.as_mut().unwrap().data.storeys[1].windows[0];
        if let Some(OP::Wall { sill_m, .. }) = &mut w.placement {
            *sill_m = sill;
        }
    }

    // The linkage invariants this test pins, checked after every re-derive.
    // Returns the at-rest placement fields for the stability assertion.
    let assert_linked = |c: &AppController, when: &str| -> (f32, f32) {
        let storey = &c.current_record.as_ref().unwrap().data.storeys[1];
        assert_eq!(storey.windows.len(), 1, "{when}: window survives");
        let w = &storey.windows[0];
        assert!(w.wall_anchor.is_some(), "{when}: durable anchor acquired");
        let Some(OpeningPlacement::Wall {
            wall_id,
            along_m,
            sill_m,
        }) = w.placement
        else {
            panic!("{when}: wall placement present");
        };
        let host = storey
            .walls
            .regions
            .iter()
            .find(|r| r.id == wall_id)
            .unwrap_or_else(|| panic!("{when}: placement.wall_id matches a live region"));
        assert_ne!(
            host.exposure,
            WallExposure::InterStorey,
            "{when}: NEVER hosted on the buried InterStorey band"
        );
        assert!(
            host.opes.windows.contains(&win_id),
            "{when}: the host region's opes contain the window"
        );
        // The host is the exposed remainder: base at the cut height.
        let base_z = host
            .base_frame()
            .unwrap_or_else(|| panic!("{when}: first-class host"))
            .3;
        assert!(
            (base_z - 1.3).abs() < 1e-3,
            "{when}: hosted on the remainder (base at the cut), got base_z {base_z}"
        );
        // The window rectangle sits in the exposed remainder (z 1.5..2.0),
        // never squashed under the buried band's top (1.3).
        assert!(!w.vertices.is_empty(), "{when}: vertices derived");
        let (zmin, zmax) = w
            .vertices
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v.2), hi.max(v.2))
            });
        assert!(
            (zmin - 1.5).abs() < 1e-3 && (zmax - 2.0).abs() < 1e-3,
            "{when}: window spans z 1.5..2.0 in the exposed remainder, got {zmin}..{zmax}"
        );
        (along_m, sill_m)
    };

    // Two full re-derives: the split + re-link must hold on every materialize,
    // and the at-rest placement fields must be IDENTICAL (the sill rebase is
    // deterministic — no ping-pong, no drift).
    c.mark_dirty().unwrap();
    c.materialize();
    let first = assert_linked(&c, "first materialize");
    c.mark_dirty().unwrap();
    c.materialize();
    let second = assert_linked(&c, "second materialize");
    assert_eq!(
        first, second,
        "placement fields identical across materializes"
    );

    // Save → reload: not pruned, still linked; and a further re-derive holds.
    let id = c.current_record.as_ref().unwrap().id;
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    assert_eq!(
        assert_linked(&c, "after reload"),
        first,
        "placement fields identical across a reload"
    );
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        assert_linked(&c, "re-derive after reload"),
        first,
        "placement fields identical after the post-reload re-derive"
    );
}

/// SELF-HEAL of a mis-hosted record (user verdict item 4): an ope persisted on
/// the buried InterStorey band (placed by the pre-fix build) whose rectangle
/// lies in the stacked exposed remainder MOVES to the remainder on the next
/// re-derive — placement, `opes`, and sill all re-homed — and stays there
/// (idempotent).
#[test]
fn mis_hosted_ope_on_the_band_self_heals_onto_the_exposed_remainder() {
    use ber_model::ber::fenestration::{Frame, Glazing, OpeningPlacement};
    use ber_model::ber::structural::WallExposure;
    let mut c = inter_storey_riser_fixture_controller();

    // Rebuild the PRE-FIX mis-hosted state directly on the record: the window
    // placement references the BAND (sill measured above the band's base =
    // the wall bottom, rectangle z 1.5..2.0 — inside the remainder), the
    // band's opes carry it, no anchor.
    let (band_id, band_base_z) = {
        let storeys = &c.current_record.as_ref().unwrap().data.storeys;
        let (band, _, _) = walls_on_x_with_extent(&storeys[1], 4.0)
            .into_iter()
            .find(|(w, _, _)| w.exposure == WallExposure::InterStorey)
            .expect("the buried band");
        (band.id, band.base_frame().expect("band base frame").3)
    };
    let type_id = c.current_record.as_mut().unwrap().data.ensure_window_type(
        Glazing::Double,
        Frame::WoodUPVC,
        0,
        false,
        Vec::new(),
        None,
    );
    let sill = (1.5 - band_base_z) as f32;
    let win_id = ber_model::ber::openings::add_wall_window(
        &mut c.current_record.as_mut().unwrap().data.storeys[1],
        band_id,
        5.0,
        1.0,
        0.5,
        sill,
        type_id,
    )
    .expect("window force-placed on the band");
    {
        use ber_model::ber::fenestration::OpeningPlacement as OP;
        let w = &mut c.current_record.as_mut().unwrap().data.storeys[1].windows[0];
        if let Some(OP::Wall { sill_m, .. }) = &mut w.placement {
            *sill_m = sill;
        }
        w.wall_anchor = None; // pre-fix records may never have acquired one
    }
    // Mis-hosted state in place: the band's opes carry the window.
    assert!(
        c.current_record.as_ref().unwrap().data.storeys[1]
            .walls
            .regions
            .iter()
            .find(|r| r.id == band_id)
            .expect("band live")
            .opes
            .windows
            .contains(&win_id),
        "fixture: the ope sits on the InterStorey band"
    );

    let assert_healed = |c: &AppController, when: &str| {
        let storey = &c.current_record.as_ref().unwrap().data.storeys[1];
        let w = &storey.windows[0];
        let Some(OpeningPlacement::Wall { wall_id, .. }) = w.placement else {
            panic!("{when}: wall placement present");
        };
        let host = storey
            .walls
            .regions
            .iter()
            .find(|r| r.id == wall_id)
            .unwrap_or_else(|| panic!("{when}: live host"));
        assert_ne!(
            host.exposure,
            WallExposure::InterStorey,
            "{when}: moved OFF the buried band"
        );
        assert!(
            host.opes.windows.contains(&win_id),
            "{when}: the exposed remainder's opes carry the window"
        );
        // Every InterStorey piece is opes-free.
        assert!(
            storey
                .walls
                .regions
                .iter()
                .filter(|r| r.exposure == WallExposure::InterStorey)
                .all(|r| r.opes.windows.is_empty() && r.opes.doors.is_empty()),
            "{when}: no ope remains on any InterStorey piece"
        );
        // The world rectangle didn't move (sill was rebased, not reset).
        let (zmin, zmax) = w
            .vertices
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v.2), hi.max(v.2))
            });
        assert!(
            (zmin - 1.5).abs() < 1e-3 && (zmax - 2.0).abs() < 1e-3,
            "{when}: rectangle unchanged at z 1.5..2.0, got {zmin}..{zmax}"
        );
    };

    c.mark_dirty().unwrap();
    c.materialize();
    assert_healed(&c, "heal on first re-derive");
    c.mark_dirty().unwrap();
    c.materialize();
    assert_healed(&c, "idempotent thereafter");
}

/// THE USER'S SCENARIO: a window is placed FIRST (low on storey 1's wall,
/// while the ground ceiling is flat — no junction there yet), and only THEN
/// the geometry changes: the ground storey gains a tall section (ceiling step
/// +1.6 at x = 4) whose volume buries the lower band of that wall — right
/// through the window's rectangle. The window must MOVE AWAY from the newly
/// buried part: the relink's nudge raises it so its bottom sits on the band
/// top (1.3 storey-local), hosted on the exposed remainder — never left
/// looking into the dwelling's own tall section — and the move is durable
/// (idempotent re-derives, identical placement fields).
#[test]
fn window_placed_before_a_burying_section_moves_up_off_the_band() {
    use ber_model::ber::fenestration::{Frame, Glazing, OpeningPlacement};
    use ber_model::ber::structural::WallExposure;
    let p = |x: f32, y: f32| Point2f32::new(x, y);

    // Ground 10×10, FLAT 2.4 ceiling; storey 1 (slab 0.3) over the left strip.
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    let ring = vec![p(0., 0.), p(4., 0.), p(4., 10.), p(0., 10.)];
    c.document.adopt_finalised_outline(&ring).expect("adopts");
    c.maybe_seed_inherited_levels();
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();

    // Storey 1's x = 4 wall is UNSPLIT (flat ground → no junction band).
    let (host_id, sill) = {
        let storeys = &c.current_record.as_ref().unwrap().data.storeys;
        let pieces = walls_on_x_with_extent(&storeys[1], 4.0);
        assert_eq!(pieces.len(), 1, "no band yet — the wall is whole");
        let host = pieces[0].0;
        let base_z = host.base_frame().expect("whole wall").3;
        // A 1×0.5 window LOW on the wall: bottom at z 0.2 (storey-local).
        (host.id, (0.2 - base_z) as f32)
    };
    let type_id = c.current_record.as_mut().unwrap().data.ensure_window_type(
        Glazing::Double,
        Frame::WoodUPVC,
        0,
        false,
        Vec::new(),
        None,
    );
    let win_id = ber_model::ber::openings::add_wall_window(
        &mut c.current_record.as_mut().unwrap().data.storeys[1],
        host_id,
        5.0,
        1.0,
        0.5,
        sill,
        type_id,
    )
    .expect("window placed on the (still whole) wall");
    {
        use ber_model::ber::fenestration::OpeningPlacement as OP;
        let w = &mut c.current_record.as_mut().unwrap().data.storeys[1].windows[0];
        if let Some(OP::Wall { sill_m, .. }) = &mut w.placement {
            *sill_m = sill;
        }
    }
    c.mark_dirty().unwrap();
    c.materialize();
    {
        // Pre-change sanity: the window sits low (z 0.2..0.7), nothing buried.
        let w = &c.current_record.as_ref().unwrap().data.storeys[1].windows[0];
        let zmin = w.vertices.iter().fold(f32::INFINITY, |lo, v| lo.min(v.2));
        assert!((zmin - 0.2).abs() < 1e-3, "window placed low, got {zmin}");
    }

    // NOW the geometry changes: back on the ground storey, raise the right
    // section by 1.6 (ceiling step at x = 4 → the tall section's volume backs
    // storey 1's x = 4 wall up to z 1.3 storey-local).
    c.set_current_storey(Some(0)).expect("switch to ground");
    c.set_ceiling_step_mode(true);
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(1.6).expect("delta applies");
    c.set_ceiling_step_mode(false);
    c.materialize();

    let assert_moved_up = |c: &AppController, when: &str| -> (f32, f32) {
        let storey = &c.current_record.as_ref().unwrap().data.storeys[1];
        assert_eq!(storey.windows.len(), 1, "{when}: window survives");
        let w = &storey.windows[0];
        let Some(OpeningPlacement::Wall {
            wall_id,
            along_m,
            sill_m,
        }) = w.placement
        else {
            panic!("{when}: wall placement present");
        };
        let host = storey
            .walls
            .regions
            .iter()
            .find(|r| r.id == wall_id)
            .unwrap_or_else(|| panic!("{when}: live host"));
        assert_ne!(
            host.exposure,
            WallExposure::InterStorey,
            "{when}: hosted on the exposed remainder, not the buried band"
        );
        assert!(
            host.opes.windows.contains(&win_id),
            "{when}: opes on the remainder"
        );
        // Moved UP, away from the buried band: bottom now ON the band top.
        let (zmin, zmax) = w
            .vertices
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v.2), hi.max(v.2))
            });
        assert!(
            (zmin - 1.3).abs() < 1e-3 && (zmax - 1.8).abs() < 1e-3,
            "{when}: window raised to z 1.3..1.8 (band top), got {zmin}..{zmax}"
        );
        (along_m, sill_m)
    };
    let first = assert_moved_up(&c, "after the burying section");
    // Durable + idempotent: a further re-derive and a reload change nothing.
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        assert_moved_up(&c, "second materialize"),
        first,
        "placement fields identical once moved"
    );
    let id = c.current_record.as_ref().unwrap().id;
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    assert_eq!(
        assert_moved_up(&c, "after reload"),
        first,
        "the move survives a reload verbatim"
    );
}

/// DOWN-nudge through the real pipeline (the "upper storey overhangs with a
/// step down" case): the ground's RIGHT section is LOWERED to 2.0 m (ceiling
/// step −0.4), a window is placed high on the ground's front wall over the
/// TALL left section (z 1.7..2.2), and THEN storey 1 is added over the whole
/// footprint. Its datum anchors on the LOWEST covered ceiling (2.0), so its
/// volume backs the front wall's top strip z∈[2.0, 2.4] over the tall run —
/// right through the window's top edge. Up can't fit (the wall ends at 2.4);
/// the window must move DOWN, its top landing on the band bottom (2.0).
#[test]
fn window_placed_before_a_step_down_overhang_moves_down_off_the_band() {
    use ber_model::ber::fenestration::{Frame, Glazing, OpeningPlacement};
    use ber_model::ber::structural::WallExposure;
    let p = |x: f32, y: f32| Point2f32::new(x, y);

    // Ground 10×10, left section 2.4 / right section 2.0 (step −0.4 at x=4).
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(-0.4).expect("delta applies");
    c.set_ceiling_step_mode(false);
    c.materialize();

    // The front wall (y = 0): place a 1×0.5 window over the TALL run, high —
    // z 1.7..2.2 (sill 1.7 above the wall base at z 0).
    let front_id = {
        let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
        storey
            .walls
            .regions
            .iter()
            .find(|w| {
                w.base_edge()
                    .is_some_and(|(a, b)| a.y.abs() < 1e-6 && b.y.abs() < 1e-6)
            })
            .expect("the front wall")
            .id
    };
    let type_id = c.current_record.as_mut().unwrap().data.ensure_window_type(
        Glazing::Double,
        Frame::WoodUPVC,
        0,
        false,
        Vec::new(),
        None,
    );
    let win_id = ber_model::ber::openings::add_wall_window(
        &mut c.current_record.as_mut().unwrap().data.storeys[0],
        front_id,
        2.0,
        1.0,
        0.5,
        1.7,
        type_id,
    )
    .expect("window placed high on the tall run");
    {
        use ber_model::ber::fenestration::OpeningPlacement as OP;
        let w = &mut c.current_record.as_mut().unwrap().data.storeys[0].windows[0];
        if let Some(OP::Wall { sill_m, .. }) = &mut w.placement {
            *sill_m = 1.7;
        }
    }
    c.mark_dirty().unwrap();
    c.materialize();
    {
        let w = &c.current_record.as_ref().unwrap().data.storeys[0].windows[0];
        let zmax = w
            .vertices
            .iter()
            .fold(f32::NEG_INFINITY, |hi, v| hi.max(v.2));
        assert!((zmax - 2.2).abs() < 1e-3, "window placed high, got {zmax}");
    }

    // NOW add storey 1 over the WHOLE footprint (slab 0.3): datum = 2.0 + 0.3,
    // so its volume's underside (2.0) buries the front wall's top strip.
    c.add_storey().unwrap();
    c.assign_floor_thickness(0.3).unwrap();
    let ring = vec![p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)];
    c.document.adopt_finalised_outline(&ring).expect("adopts");
    c.maybe_seed_inherited_levels();
    c.set_default_ceiling_height(2.4).unwrap();
    c.materialize();

    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    // The band exists on the front wall (its tall-run top strip is buried).
    assert!(
        storey
            .walls
            .regions
            .iter()
            .any(|r| r.exposure == WallExposure::InterStorey
                && r.base_edge()
                    .is_some_and(|(a, b)| a.y.abs() < 1e-6 && b.y.abs() < 1e-6)),
        "the front wall carries a buried top strip"
    );
    let w = &storey.windows[0];
    let Some(OpeningPlacement::Wall {
        wall_id, sill_m, ..
    }) = w.placement
    else {
        panic!("wall placement present");
    };
    let host = storey
        .walls
        .regions
        .iter()
        .find(|r| r.id == wall_id)
        .expect("live host");
    assert_ne!(
        host.exposure,
        WallExposure::InterStorey,
        "hosted on the labellable piece, not the buried strip"
    );
    assert!(host.opes.windows.contains(&win_id), "opes on the host");
    assert!(
        (sill_m - 1.5).abs() < 1e-3,
        "moved DOWN: top onto the band bottom (sill 1.5), got {sill_m}"
    );
    let (zmin, zmax) = w
        .vertices
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v.2), hi.max(v.2))
        });
    assert!(
        (zmin - 1.5).abs() < 1e-3 && (zmax - 2.0).abs() < 1e-3,
        "window dropped to z 1.5..2.0, got {zmin}..{zmax}"
    );
    // Idempotent thereafter.
    c.mark_dirty().unwrap();
    c.materialize();
    let w = &c.current_record.as_ref().unwrap().data.storeys[0].windows[0];
    let Some(OpeningPlacement::Wall { sill_m: s2, .. }) = w.placement else {
        panic!("wall placement present");
    };
    assert_eq!(s2, sill_m, "clear once — never moved again");
}
