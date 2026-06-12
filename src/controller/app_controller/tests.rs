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
fn dimensioned_square_controller() -> AppController {
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
        c.skip_position_dimension();
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

    c.skip_position_dimension(); // X — accept as drawn
    assert!(!c.step_pick_pending(), "Y still pending");
    c.skip_position_dimension(); // Y — accept as drawn
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
    let base = |reg: &datatypes::ber::structural::WallRegion| {
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
    use datatypes::ber::structural::WallConstructionType;
    let mut c = dimensioned_square_controller();
    let (_, a0, b0) = c.document.outline.floor_plan.perimeter_walls()[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
    let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();

    c.set_exposure_mode(true);
    // Mark wall0 whole so it derives to a region we can attach a construction to.
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_wall_exposure(WallExposure::Exposed);
    // Point wall0's region at a known (non-default) shared construction, and
    // give that construction a description.
    c.materialize(); // derive wall0's region before attaching a construction
    let cid = {
        let record = c.current_record.as_mut().unwrap();
        let cid = record.data.ensure_wall_construction(
            WallConstructionType::CavityWall,
            Default::default(),
            Default::default(),
        );
        record
            .data
            .wall_constructions
            .iter_mut()
            .find(|wc| wc.base.id == cid)
            .unwrap()
            .base
            .description = Some("party".into());
        record.data.storeys[0].walls.regions[0].construction_id = cid;
        cid
    };

    // Split wall0: mark the middle semi-exposed.
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_exposure_section_range(len * 0.2, len * 0.5);
    c.set_wall_exposure(WallExposure::SemiExposed);

    // wall0 is now 3 regions (derived first) — all inherit the SAME shared
    // construction id; only exposure differs.
    c.materialize(); // edits defer the derive; bring current_record current
    let record = c.current_record.as_ref().unwrap();
    let r = &record.data.storeys[0].walls.regions;
    for i in 0..3 {
        assert_eq!(r[i].construction_id, cid, "piece {i} keeps construction");
    }
    // The shared construction still carries its type + description.
    let wc = record
        .data
        .wall_constructions
        .iter()
        .find(|wc| wc.base.id == cid)
        .unwrap();
    assert_eq!(wc.construction_type, WallConstructionType::CavityWall);
    assert_eq!(wc.base.description.as_deref(), Some("party"));
    assert_eq!(r[1].exposure, WallExposure::SemiExposed, "middle is semi");
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
    use datatypes::ber::common::ConstructionPhases;
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
/// walls outside stay `Original`.
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
            assert_eq!(
                phase,
                ConstructionPhase::Original,
                "wall {i} (other) original"
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
    assert!(phases.contains(&ConstructionPhase::Original));
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

/// A later phase polygon overwrites an existing non-`Original` phase under it
/// (polygon always wins; latest tag first).
#[test]
fn polygon_overwrites_existing_phase() {
    let p = |x: f32, y: f32| Point2f32::new(x, y);
    let right = vec![p(6., -1.), p(12., -1.), p(12., 12.), p(6., 12.)];
    let mut c = dimensioned_square_controller();
    c.set_all_remaining_exposed();
    let regions = c.phase_regions.entry(0).or_default();
    regions.push(PhaseRegion {
        polygon: right.clone(),
        phase: ConstructionPhase::Extension2,
    });
    c.commit_to_ber().unwrap();
    // Now tag the SAME area Extension1 — must override the Extension2.
    c.phase_regions.entry(0).or_default().push(PhaseRegion {
        polygon: right,
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
    assert_eq!(wall_phase(&c, right_idx), ConstructionPhase::Extension1);
}

/// Once a wall is tagged, removing the polygon does NOT revert it — a surface
/// keeps its non-`Original` phase when no polygon covers it (the durable
/// record is `construction_id`). This is the reload-safety guarantee.
#[test]
fn preserve_existing_phase_when_no_polygon() {
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

    // Drop all polygons (as a reload/new session would) and re-commit.
    c.phase_regions.clear();
    c.commit_to_ber().unwrap();
    assert_eq!(
        wall_phase(&c, right_idx),
        ConstructionPhase::Extension1,
        "tagged wall keeps its phase with no polygon"
    );
    assert_eq!(
        wall_phase(&c, left_idx),
        ConstructionPhase::Original,
        "untagged wall stays original"
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
    c.exposure_pointer_drag(mid.0 as f64, mid.1 as f64);
    let sel1 = c.exposure_selection().unwrap();
    assert!(
        (sel1.end_m - len * 0.5).abs() < 0.1,
        "dragged end to mid: {}",
        sel1.end_m
    );
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
/// and the next materialize derives the roofs at that height (one flat
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
    assert_eq!(storey.roofs.regions.len(), 1, "one region per outline ring");
    assert!(
        storey.roofs.regions[0]
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
fn roof_flat_zs(c: &AppController) -> Vec<f64> {
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let mut zs: Vec<f64> = storey
        .roofs
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
    assert!(dump.contains("roofs (2):"), "{dump}");
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
    let zs = roof_flat_zs(&c);
    assert_eq!(zs.len(), 2, "roof split into two flat pieces");
    assert!((zs[0] - 2.4).abs() < 1e-6, "default piece keeps h");
    assert!((zs[1] - 2.7).abs() < 1e-6, "stepped piece at h + Δ");
    // The RAISED piece is the one right of the line.
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    for region in &storey.roofs.regions {
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
    let zs = roof_flat_zs(&c);
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
    c.skip_position_dimension();
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

/// Reload reconstructs the session regions from the persisted roofs, and a
/// no-edit re-derive keeps the split — the canonicalize-at-commit fixpoint.
#[test]
fn ceiling_step_split_survives_reload_and_rederive() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).unwrap();
    c.materialize();
    let before = roof_flat_zs(&c);
    assert_eq!(before.len(), 2);

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    // Session regions rebuilt from the persisted roofs.
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "reconstructed from the stepped roof");
    assert!((session[0].delta_m - 0.3).abs() < 1e-6);

    // Re-derive without any edit: the split must be reproduced exactly
    // (derive → reconstruct → derive is a fixpoint).
    c.mark_dirty().unwrap();
    c.materialize();
    let after = roof_flat_zs(&c);
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
    assert_eq!(roof_flat_zs(&c).len(), 2);

    c.delete_ceiling_step_region(0);
    c.materialize();
    let zs = roof_flat_zs(&c);
    assert!(
        zs.iter().all(|z| (z - 2.4).abs() < 1e-6),
        "roof back at the default everywhere (got {zs:?})"
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

/// The assign validation: a near-zero step (|Δ| < 0.01 m) and a drop at or
/// past the default height are rejected, with the pending line left intact
/// so the user can retype; a valid amount still lands afterwards.
#[test]
fn ceiling_step_amount_rejections_keep_the_pending_line() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);

    assert!(
        c.assign_ceiling_step_delta(0.005).is_err(),
        "near-zero rejected"
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
    assert_eq!(roof_flat_zs(&c).len(), 2);
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
    assert_eq!(
        tag.current_kind,
        Some(manip_2d_plan::SnapKind::ToParallelWall)
    );
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
    c.skip_position_dimension();
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
fn roof_ready_controller() -> AppController {
    let mut c = dimensioned_square_controller();
    c.set_default_ceiling_height(2.4).expect("height applies");
    c.set_roof_point_mode(true);
    assert!(c.roof_point_mode(), "mode enters once available");
    c
}

/// The mode toggle is gated exactly like the ceiling steps: refused while the
/// storey has no default ceiling height, available once it's set, and never
/// available on the read-only All tab. A tap outside the footprint is
/// rejected (nothing goes pending).
#[test]
fn roof_point_mode_gated_like_ceiling_steps() {
    let mut c = dimensioned_square_controller();
    assert!(!c.roof_point_available(), "no default height yet");
    c.set_roof_point_mode(true);
    assert!(!c.roof_point_mode(), "setter re-enforces the gate");

    c.set_default_ceiling_height(2.4).unwrap();
    assert!(c.roof_point_available());
    c.set_roof_point_mode(true);
    assert!(c.roof_point_mode());

    // A tap outside the footprint bbox is rejected up front.
    c.roof_point_tap(15.0, 5.0);
    assert!(!c.roof_point_pending(), "off-footprint tap rejected");

    // The All tab is read-only — not available there (the switch also resets
    // the mode like the other area tools).
    c.set_current_storey(None).unwrap();
    assert!(!c.roof_point_available());
    assert!(!c.roof_point_mode());
}

/// The full un-snapped flow: tap → X position prompt (kind `Point`, the
/// "area wall" degenerate at the tapped point) → Y prompt → height → assign
/// FANS the containing ceiling region: one triangle per boundary edge, every
/// triangle holding the apex vertex at the entered height, boundary corners
/// at the base. The applied positions land exactly; the session ceiling-step
/// list stays EMPTY (a fan over the default base reconstructs to nothing —
/// the apex is durable input, not session state).
#[test]
fn roof_point_tap_x_y_height_flow_fans_the_region() {
    let mut c = roof_ready_controller();
    c.roof_point_tap(3.8, 5.2);
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
    assert!(c.roof_point_height_pending(), "both axes pinned → height");
    assert_eq!(
        c.pending_roof_point_position(),
        Some((4.0, 5.0)),
        "applied positions land exactly"
    );

    c.assign_roof_apex(4.2).expect("apex assigns");
    assert!(!c.roof_point_pending(), "flow complete");
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(
        storey.roofs.regions.len(),
        4,
        "one fan triangle per boundary edge"
    );
    for region in &storey.roofs.regions {
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
    assert_eq!(storey.roof_apexes.len(), 1, "the durable input");
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "a fan over the default base needs NO session step state"
    );
    assert_eq!(c.roof_apex_views(), vec![((4.0, 5.0), "▲ 4.20 m".into())]);
}

/// A tap within snap tolerance of a parallel wall on ONE axis snaps that
/// axis: its position prompt is SKIPPED (the coordinate is a reference
/// wall's, exact by construction), the single tag renders, the OTHER axis is
/// still asked, and the assigned apex lands exactly on the wall's coordinate.
#[test]
fn roof_point_snapped_axis_skips_its_prompt_and_lands_exactly() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    assert!(c.roof_point_mode());
    // Tap (3.0, 3.9): Y is within 0.2 of the interior wall y = 4 → snaps;
    // X has no wall within tolerance → asked.
    c.roof_point_tap(3.0, 3.9);
    let tag = c.roof_point_snap_tag().expect("snap tag present");
    assert_eq!(
        tag.current_kind,
        Some(manip_2d_plan::SnapKind::ToParallelWall)
    );
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
        c.roof_point_height_pending(),
        "snapped Y skipped its prompt — straight to the height"
    );
    assert!(c.roof_point_snap_tag().is_some(), "tag still clickable");

    c.assign_roof_apex(4.0).expect("apex assigns");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let apex = &storey.roof_apexes[0];
    assert!((apex.point.x - 3.5).abs() < 1e-6);
    assert!(
        (apex.point.y - 4.0).abs() < 1e-6,
        "snapped axis lands EXACTLY"
    );
}

/// Both axes within tolerance = a corner snap for free: no position prompt
/// at all, straight to the height, and the single tag shows the corner kind.
#[test]
fn roof_point_both_axes_snapped_goes_straight_to_height() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    c.roof_point_tap(5.9, 3.9); // within 0.2 of x = 6 AND y = 4
    assert!(
        c.position_prompt_for_ui().is_none(),
        "both snapped — no position question"
    );
    assert!(c.roof_point_height_pending(), "straight to the height");
    assert_eq!(
        c.pending_roof_point_position(),
        Some((6.0, 4.0)),
        "both coordinates snapped"
    );
    let tag = c.roof_point_snap_tag().expect("one tag for both");
    assert_eq!(tag.current_kind, Some(manip_2d_plan::SnapKind::ToCorner));
}

/// "Don't snap" from the HEIGHT stage reverts BOTH axes to their tapped
/// coordinates and restarts the question flow from X.
#[test]
fn unsnap_roof_point_restarts_from_x_with_the_drawn_coords() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    c.roof_point_tap(5.9, 3.9);
    assert!(c.roof_point_height_pending());

    c.unsnap_roof_point();
    assert!(c.roof_point_snap_tag().is_none(), "snaps dropped");
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
    c.skip_position_dimension();
    let ui = c.position_prompt_for_ui().expect("Y question follows");
    assert_eq!(ui.axis, ParameterAxis::Y);
    c.skip_position_dimension();
    assert!(c.roof_point_height_pending());
}

/// Reload rebuilds the fan from the DURABLE apex with NO session state, and
/// a no-edit re-derive reproduces it exactly — the apex reload fixpoint.
#[test]
fn roof_apex_survives_reload_with_no_session_state_and_rederive_fixpoint() {
    let mut c = roof_ready_controller();
    c.roof_point_tap(3.8, 5.2);
    c.apply_position_dimension(4.0).unwrap();
    c.apply_position_dimension(5.0).unwrap();
    c.assign_roof_apex(4.2).unwrap();
    c.materialize();

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.roofs.regions.len(), 4, "fan persisted");
    assert_eq!(storey.roof_apexes.len(), 1, "apex input persisted");
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "no session state to reconstruct for a default-base fan"
    );

    // Re-derive without any edit: the fan must be reproduced exactly.
    c.mark_dirty().unwrap();
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(
        storey.roofs.regions.len(),
        4,
        "fan kept across the re-derive"
    );
    let max_z = storey
        .roofs
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
fn roof_apex_over_a_ceiling_step_canonicalizes_and_survives_reload() {
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).unwrap(); // right of x = 4 at 2.7
    c.set_roof_point_mode(true); // leaves ceiling-step mode; sessions stay

    // Apex inside the STEPPED piece, at its drawn position (skip both axes).
    c.roof_point_tap(7.0, 5.0);
    c.skip_position_dimension(); // X stays 7.0
    c.skip_position_dimension(); // Y stays 5.0
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
            .roofs
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
    assert_eq!(session.len(), 1, "merged area reconstructed from the roofs");
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
    let mut c = roof_ready_controller();
    c.roof_point_tap(3.8, 5.2);
    c.apply_position_dimension(4.0).unwrap();
    c.apply_position_dimension(5.0).unwrap();
    c.assign_roof_apex(4.2).unwrap();
    c.materialize();
    assert_eq!(
        c.current_record.as_ref().unwrap().data.storeys[0]
            .roofs
            .regions
            .len(),
        4
    );

    c.delete_roof_apex(0);
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(storey.roof_apexes.is_empty(), "durable input removed");
    assert_eq!(storey.roofs.regions.len(), 1, "back to one flat region");
    assert!(
        storey
            .roofs
            .regions
            .iter()
            .all(|r| r.flat_z().is_some_and(|z| (z - 2.4).abs() < 1e-6)),
        "flat at the default again"
    );
    assert!(c.ceiling_step_regions.get(&0).is_none());
    // Out-of-range delete is a no-op.
    c.delete_roof_apex(7);
}

/// A storey switch abandons the in-flight point and leaves the mode — its
/// coordinates are in the outgoing storey's space.
#[test]
fn storey_switch_clears_pending_roof_point() {
    let mut c = roof_ready_controller();
    c.roof_point_tap(3.8, 5.2);
    assert!(c.roof_point_pending());
    c.add_storey().unwrap();
    assert!(!c.roof_point_mode(), "mode reset on switch");
    assert!(!c.roof_point_pending(), "pending point abandoned");
}

/// The two-point ridge flow at the shell level: a SECOND apex inside an
/// already-fanned region is ACCEPTED — the ridge forms (a sloping one here,
/// the two heights differ) — and a THIRD point in the ridged area is
/// rejected with the stable two-per-area message; the pending point stays
/// open so the user can cancel or move on.
#[test]
fn second_roof_point_forms_a_ridge_and_a_third_is_rejected() {
    let mut c = roof_ready_controller();
    c.roof_point_tap(3.8, 5.2);
    c.apply_position_dimension(4.0).unwrap();
    c.apply_position_dimension(5.0).unwrap();
    c.assign_roof_apex(4.2).unwrap();

    // Second point, strictly inside one of the fan triangles: the ridge.
    c.roof_point_tap(3.9, 1.1);
    c.skip_position_dimension();
    c.skip_position_dimension();
    c.assign_roof_apex(3.5)
        .expect("second point forms the ridge");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.roof_apexes.len(), 2, "both apexes durable");
    let zs: Vec<f64> = storey
        .roofs
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
    c.roof_point_tap(8.0, 5.0);
    c.skip_position_dimension();
    c.skip_position_dimension();
    assert!(c.roof_point_height_pending());
    let err = c.assign_roof_apex(3.0).unwrap_err();
    assert!(err.contains("already has a ridge"), "got: {err}");
    assert!(c.roof_point_height_pending(), "pending intact on rejection");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.roof_apexes.len(), 2, "nothing committed");
    c.cancel_pending_roof_point();
    assert!(!c.roof_point_pending());
}

// ─── Roof ridges (line input: length + cross + start questions + height) ───

/// Sorted (x, y, z) vertex triples of storey 0's derived roof regions —
/// a strong geometric fingerprint for "same roof" comparisons.
fn roof_vertex_fingerprint(c: &AppController) -> Vec<(f64, f64, f64)> {
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    let mut vs: Vec<(f64, f64, f64)> = storey
        .roofs
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
fn roof_ridge_length_cross_start_height_flow_forms_a_pitched_roof() {
    let mut c = roof_ready_controller();
    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);

    // Question 1: the LENGTH — prefilled with the drawn |hi − lo|, no
    // reference wall (it's the ridge's own dimension).
    assert!(
        c.roof_ridge_length_pending(),
        "stroke fitted → length first"
    );
    assert!(
        c.position_prompt_for_ui().is_none(),
        "the length is not a position question"
    );
    let prefill = c.pending_roof_ridge_length().expect("drawn length");
    assert!((prefill - 6.0).abs() < 1e-4);
    assert!(
        !c.roof_ridge_length_anchored(),
        "nothing snapped — no anchored end"
    );
    // A typed length anchors at the drawn lo: the extent becomes [2, 6].
    c.assign_roof_ridge_length(4.0).expect("length applies");

    // Question 2: the CROSS position.
    assert!(c.roof_ridge_position_pending(), "length resolved → cross");
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
    assert!(c.roof_ridge_position_pending(), "cross resolved → start");
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
    assert!(c.roof_ridge_height_pending(), "start resolved → height");
    let ((_, ay), (_, by)) = c.pending_roof_ridge_line().unwrap();
    assert!(
        (ay - 3.0).abs() < 1e-4 && (by - 7.0).abs() < 1e-4,
        "extent = [typed start, typed start + typed length]"
    );
    c.assign_roof_ridge_height(4.2).expect("ridge assigns");
    assert!(!c.roof_ridge_pending(), "flow complete");
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.roof_apexes.len(), 2, "both apexes durable");
    for (apex, want_y) in storey.roof_apexes.iter().zip([3.0, 7.0]) {
        assert!((apex.point.x - 4.0).abs() < 1e-6);
        assert!(
            (apex.point.y - want_y).abs() < 1e-4,
            "apexes at the RESOLVED start/length, not the drawn ones"
        );
        assert!((apex.z_m - 4.2).abs() < 1e-6);
    }
    let groups = storey.roofs.apex_groups(&storey.roof_apexes);
    assert_eq!(groups.len(), 1, "one connected roof group");
    assert_eq!(
        groups[0].apex_idxs.len(),
        2,
        "…holding BOTH apexes: a ridge"
    );
    let zs: Vec<f64> = storey
        .roofs
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
        c.roof_ridge_views(),
        vec![((4.0, 3.0), (4.0, 7.0), "⌃ 4.20 m".into())],
        "committed overlay: the segment + level-ridge label"
    );
}

/// The line input and the two-point tap flow are the SAME model: skipping
/// every question keeps the DRAWN values (cross, start, AND length), and
/// placing the two apexes by taps at exactly those drawn endpoints derives a
/// vertex-for-vertex identical roof.
#[test]
fn roof_ridge_line_input_matches_the_two_point_flow() {
    // Line input with every prompt skipped: the drawn segment stands.
    let mut line = roof_ready_controller();
    line.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    let ((ax, ay), (bx, by)) = line.pending_roof_ridge_line().expect("drawn segment");
    line.skip_roof_ridge_length().unwrap(); // length: drawn |hi − lo| stands
    line.skip_position_dimension(); // cross: drawn coordinate stands
    line.skip_position_dimension(); // start: drawn lo stands
    line.assign_roof_ridge_height(4.2).unwrap();
    line.materialize();
    let ((lax, lay), (lbx, lby)) = ((ax, ay), (bx, by));

    // Two-point flow at the same (drawn) coordinates and height.
    let mut taps = roof_ready_controller();
    for (x, y) in [(lax, lay), (lbx, lby)] {
        taps.roof_point_tap(x as f32, y as f32);
        taps.skip_position_dimension(); // X stays as tapped
        taps.skip_position_dimension(); // Y stays as tapped
        taps.assign_roof_apex(4.2).expect("apex assigns");
    }
    taps.materialize();

    let a = roof_vertex_fingerprint(&line);
    let b = roof_vertex_fingerprint(&taps);
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
fn roof_ridge_snap_skips_the_cross_question_and_lands_on_the_wall() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    assert!(c.roof_point_mode());
    // Drawn at x ≈ 5.9 — within 0.2 of the interior wall at x = 6; the
    // extent (y 0.5..3.5) stays inside the L's lower band, beyond snap
    // tolerance of any horizontal wall.
    c.apply_roof_ridge_stroke_event(&[(5.88, 0.5), (5.92, 2.0), (5.9, 3.5)]);
    assert!(c.roof_ridge_length_pending(), "length is always question 1");
    let ((ax, _), _) = c.pending_roof_ridge_line().unwrap();
    assert!((ax - 6.0).abs() < 1e-6, "coordinate snapped to the wall");
    assert!(
        c.roof_ridge_snap_tag().is_none(),
        "no tag at the Length stage — no snap question passed yet"
    );

    c.skip_roof_ridge_length().unwrap(); // length: drawn length stands
    let ui = c
        .position_prompt_for_ui()
        .expect("cross snapped → the START question opens directly");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    assert_eq!(ui.axis, ParameterAxis::Y, "the along axis's question");

    let tag = c.roof_ridge_snap_tag().expect("snap tag present");
    assert_eq!(
        tag.current_kind,
        Some(manip_2d_plan::SnapKind::ToParallelWall)
    );
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

    c.skip_position_dimension(); // start: drawn lo stands → finalize
    assert!(c.roof_ridge_height_pending());
    assert!(c.roof_ridge_snap_tag().is_some(), "tag still clickable");
    c.assign_roof_ridge_height(4.0).expect("ridge assigns");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.roof_apexes.len(), 2);
    for (apex, want_y) in storey.roof_apexes.iter().zip([0.5, 3.5]) {
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
fn roof_ridge_start_snap_skips_its_question_and_unsnap_reopens_it() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    // Vertical stroke at x ≈ 2.8 (no parallel wall within 0.2), starting at
    // y ≈ 4.1 — within 0.2 of the horizontal wall y = 4 — ending at y = 8.5.
    c.apply_roof_ridge_stroke_event(&[(2.78, 4.1), (2.82, 6.0), (2.8, 8.5)]);

    // Question 1 (length) opens first; the snapped start ANCHORS the
    // extent (a typed length extends from the exact wall coordinate); no
    // tag yet — the stage owns the tag, and no snap question has arrived.
    assert!(c.roof_ridge_length_pending(), "length first");
    assert!(c.roof_ridge_length_anchored(), "start snapped → anchored");
    assert!(c.roof_ridge_snap_tag().is_none(), "no tag at the length");
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
    assert!(c.roof_ridge_snap_tag().is_none(), "no tag at the cross");
    c.apply_position_dimension(2.5).expect("cross applies"); // from x = 0

    // The start question is SKIPPED (snapped) → finalize → height.
    assert!(
        c.position_prompt_for_ui().is_none(),
        "snapped start skips its question"
    );
    assert!(c.roof_ridge_height_pending(), "straight to the height");
    let ((ax, ay), _) = c.pending_roof_ridge_line().unwrap();
    assert!(
        (ax - 2.5).abs() < 1e-6 && (ay - 4.0).abs() < 1e-6,
        "snapped"
    );
    let tag = c.roof_ridge_snap_tag().expect("start tag present");
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
    assert!(c.roof_ridge_snap_tag().is_none(), "snap dropped");
    let ui = c.position_prompt_for_ui().expect("start question reopened");
    assert_eq!(ui.axis, ParameterAxis::Y);
    let ((ax, ay), _) = c.pending_roof_ridge_line().unwrap();
    assert!((ax - 2.5).abs() < 1e-6, "resolved cross kept");
    assert!((ay - 4.1).abs() < 0.05, "back at the DRAWN start");

    // Type the start (1 m from the nearest ref y = 4, drawn side +): the
    // apexes land at the RESOLVED start with the resolved (drawn) length.
    c.apply_position_dimension(1.0).expect("start applies"); // y = 5
    c.assign_roof_ridge_height(4.0).expect("ridge assigns");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    for (apex, want_y) in storey.roof_apexes.iter().zip([5.0, 9.4]) {
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
    c.skip_position_dimension();
    assert!(c.roof_ridge_height_pending());
    assert!(c.roof_ridge_snap_tag().is_some(), "cross tag at the height");

    c.unsnap_roof_ridge();
    assert!(c.roof_ridge_snap_tag().is_none(), "snap dropped");
    let ui = c.position_prompt_for_ui().expect("cross question reopened");
    assert_eq!(ui.kind, PositionPromptKind::Line);
    assert_eq!(ui.axis, ParameterAxis::X, "restarted from the cross");
    let ((ax, _), _) = c.pending_roof_ridge_line().unwrap();
    assert!((ax - 5.9).abs() < 0.05, "back at the DRAWN coordinate");

    c.skip_position_dimension(); // cross: drawn stands
    c.skip_position_dimension(); // start re-asked: drawn stands
    assert!(
        c.roof_ridge_height_pending(),
        "skips → height, drawn values (the resolved length stood)"
    );
    c.assign_roof_ridge_height(4.0).expect("ridge assigns");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(
        storey
            .roof_apexes
            .iter()
            .all(|a| (a.point.x - 5.9).abs() < 0.05),
        "assigned at the drawn coordinate"
    );
}

/// Reload rebuilds the ridge from the DURABLE apex pair with no session
/// state, and a no-edit re-derive reproduces the surface exactly — the ridge
/// reload fixpoint.
#[test]
fn roof_ridge_survives_reload_and_rederive_fixpoint() {
    let mut c = roof_ready_controller();
    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    c.assign_roof_ridge_length(6.0).unwrap();
    c.apply_position_dimension(4.0).unwrap(); // cross
    c.apply_position_dimension(2.0).unwrap(); // start (y = 2)
    c.assign_roof_ridge_height(4.2).unwrap();
    c.materialize();
    let before = roof_vertex_fingerprint(&c);

    let id = c.current_record.as_ref().unwrap().id.clone();
    c.flush_if_dirty().unwrap();
    c.open_ber(&id).unwrap();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.roof_apexes.len(), 2, "apex pair persisted");
    let groups = storey.roofs.apex_groups(&storey.roof_apexes);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].apex_idxs.len(), 2, "still a ridge after reload");
    assert!(
        c.ceiling_step_regions.get(&0).is_none(),
        "no session state to reconstruct for a default-base ridge"
    );
    assert_eq!(c.roof_ridge_views().len(), 1, "overlay re-derives");

    // Re-derive without any edit: the ridge must be reproduced exactly.
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        roof_vertex_fingerprint(&c),
        before,
        "derive → reload → derive is a fixpoint"
    );
}

/// THE BOUNDARY-RIDGE FULL FIXPOINT (gables): a wall-to-wall ridge over a
/// STEPPED piece — the apexes sit exactly ON the outline, the derived step
/// ring is boolean-op output (the noisy path the EPS claim band exists for).
/// Resolve → commit-style reconstruct (canonicalize the session step list
/// from the derived roofs) → persist → reload → re-resolve must reproduce
/// the surface identically, and the wall under each ridge end must carry the
/// gable apex on its top. (The apexes are stamped directly — the typed-start
/// flow can't reach the boundary until the end snaps land; this is the
/// durable-input path a snap will drive.)
#[test]
fn boundary_ridge_over_a_step_full_fixpoint_and_gable_walls() {
    use datatypes::ber::structural::RoofApex;
    let mut c = ceiling_ready_controller();
    ceiling_step_to_amount_at_x4(&mut c);
    c.assign_ceiling_step_delta(0.3).unwrap(); // right of x = 4 at 2.7

    // The wall-to-wall gable ridge over the stepped piece: ends ON the
    // outline walls y = 0 / y = 10 at x = 7.
    let record = c.current_record.as_mut().unwrap();
    for y in [0.0, 10.0] {
        record.data.add_roof_apex(
            0,
            RoofApex {
                point: Point2f64::new(7.0, y),
                z_m: 4.5,
            },
        );
    }
    c.mark_dirty().unwrap();
    // Commit-style canonicalize: derive, then rebuild the session step list
    // from the derived roofs (exactly assign_roof_ridge_height's shape).
    c.materialize();
    c.reconstruct_ceiling_step_regions_for(0);
    let before = roof_vertex_fingerprint(&c);
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
    let groups = storey.roofs.apex_groups(&storey.roof_apexes);
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
    assert_eq!(storey.roof_apexes.len(), 2, "apex pair persisted");
    let session = &c.ceiling_step_regions[&0];
    assert_eq!(session.len(), 1, "merged area reconstructed");
    c.mark_dirty().unwrap();
    c.materialize();
    assert_eq!(
        roof_vertex_fingerprint(&c),
        before,
        "resolve → reconstruct → re-resolve is a fixpoint"
    );
}

/// Deleting ONE apex of a ridge degrades it to a fan on the next derive —
/// the group keeps the remaining apex, and the ridge overlay disappears.
#[test]
fn deleting_one_ridge_apex_degrades_to_a_fan() {
    let mut c = roof_ready_controller();
    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    c.skip_roof_ridge_length().unwrap();
    c.apply_position_dimension(4.0).unwrap(); // cross
    c.skip_position_dimension(); // start: drawn stands
    c.assign_roof_ridge_height(4.2).unwrap();
    c.materialize();

    c.delete_roof_apex(0);
    c.materialize();
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.roof_apexes.len(), 1, "one apex left");
    let groups = storey.roofs.apex_groups(&storey.roof_apexes);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].apex_idxs.len(), 1, "ridge degraded to a fan");
    assert!(c.roof_ridge_views().is_empty(), "no two-apex overlay left");
    let zs: Vec<f64> = storey
        .roofs
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
fn roof_ridge_rejections_keep_the_pending_and_bad_strokes_never_pend() {
    let mut c = roof_ready_controller();
    // A stroke outside the footprint is rejected up front.
    c.apply_roof_ridge_stroke_event(&[(15.0, 2.0), (15.0, 5.0), (15.0, 8.0)]);
    assert!(!c.roof_ridge_pending(), "off-footprint stroke rejected");

    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    assert!(c.roof_ridge_length_pending(), "length is question 1");
    // A tap while the ridge is pending must not spawn a roof point.
    c.roof_point_tap(7.0, 5.0);
    assert!(!c.roof_point_pending(), "tap suppressed while ridge pends");

    // Length rejections: zero-ish, then beyond every area's bbox (the
    // 10×10 footprint's one flat region runs 10 m along the ridge axis) —
    // with the permitted bound in the message.
    let err = c.assign_roof_ridge_length(0.0).unwrap_err();
    assert!(err.contains("more than a centimetre"), "got: {err}");
    let err = c.assign_roof_ridge_length(11.0).unwrap_err();
    assert!(err.contains("No ceiling area is that long"), "got: {err}");
    assert!(err.contains("under 10.00 m"), "got: {err}");
    assert!(c.roof_ridge_length_pending(), "pending intact on rejection");

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
        c.roof_ridge_position_pending(),
        "start prompt stays open on the finalize rejection"
    );
    // A start the length fits from (y = 0.5: the extent ends 0.5 m clear
    // of the far wall) lands → height.
    c.apply_position_dimension(0.5)
        .expect("fitting start lands");
    assert!(c.roof_ridge_height_pending());

    let err = c.assign_roof_ridge_height(2.405).unwrap_err();
    assert!(err.contains("within a centimetre"), "got: {err}");
    assert!(c.roof_ridge_height_pending(), "pending intact on rejection");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert!(storey.roof_apexes.is_empty(), "nothing committed");

    c.assign_roof_ridge_height(4.2).expect("valid height lands");
    assert!(!c.roof_ridge_pending());
}

/// THE STAGE-ORDER TABLE (design §3, normative): Length is question 1 and
/// is always asked; the Cross and Start questions follow, each SKIPPED when
/// its degree of freedom snapped at stroke time; the finalize placement
/// check runs at the last open stage; then Height. One row per snap
/// combination, driven by where the stroke lands in the L footprint (the
/// interior wall x = 6 and the horizontal wall y = 4 are the snap targets).
#[test]
fn roof_ridge_stage_order_table() {
    // The pending ridge's stage as a short label. The two position
    // questions are told apart by the open prompt's axis — the table uses
    // VERTICAL strokes only, so X is the cross question, Y the start.
    fn stage(c: &AppController) -> &'static str {
        if c.roof_ridge_length_pending() {
            return "len";
        }
        if c.roof_ridge_height_pending() {
            return "height";
        }
        if c.roof_ridge_position_pending() {
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
    let rows: [(&str, [(f32, f32); 2], &[&str]); 3] = [
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
    ];
    for (name, stroke, want) in rows {
        let mut c = ceiling_ready_l_controller();
        c.set_roof_point_mode(true);
        c.apply_roof_ridge_stroke_event(&stroke);
        let mut seq: Vec<&str> = Vec::new();
        let mut guard = 0;
        while c.roof_ridge_pending() && guard < 6 {
            let s = stage(&c);
            seq.push(s);
            match s {
                "len" => c
                    .skip_roof_ridge_length()
                    .unwrap_or_else(|e| panic!("{name}: length skip rejected: {e}")),
                "cross" | "start" => c.skip_position_dimension(),
                "height" => break,
                other => panic!("{name}: unexpected stage {other}"),
            }
            guard += 1;
        }
        assert_eq!(&seq, want, "{name}");
    }
}

/// The ALL-SNAPPED row of the stage table: with BOTH positions snapped no
/// location question opens, so the finalize placement check runs straight
/// off the Length stage and its rejection lands on the typed length. In the
/// L footprint the cross snap (x ≈ 5.9 → 6) and start snap (y ≈ 4.1 → 4)
/// put the ridge ALONG the upper arm's right wall x = 6 — its midpoint
/// sits on the boundary, in no ceiling area — which finalize rejects; the
/// Length stage stays open (Cancel recovers). (A boundary START is legal
/// now — the gable feature — but a ridge along a wall still makes no roof.)
#[test]
fn roof_ridge_all_snapped_finalize_rejection_lands_on_the_length() {
    let mut c = ceiling_ready_l_controller();
    c.set_roof_point_mode(true);
    // x ≈ 5.9 → snaps 6; lo ≈ 4.1 → snaps 4: the ridge runs up the wall
    // x = 6 from the corner (6, 4).
    c.apply_roof_ridge_stroke_event(&[(5.88, 4.1), (5.9, 8.5)]);
    assert!(c.roof_ridge_length_pending(), "length is question 1");
    let ((ax, ay), _) = c.pending_roof_ridge_line().unwrap();
    assert!(
        (ax - 6.0).abs() < 1e-6 && (ay - 4.0).abs() < 1e-6,
        "both snaps landed"
    );
    let err = c.assign_roof_ridge_length(2.0).unwrap_err();
    assert!(
        err.contains("doesn't sit inside a ceiling area"),
        "the finalize midpoint-containment rejection, on the typed length: {err}"
    );
    assert!(c.roof_ridge_length_pending(), "length stage stays open");
    // The skip path trips the same check (logged + surfaced to the caller).
    let err = c.skip_roof_ridge_length().unwrap_err();
    assert!(err.contains("doesn't sit inside a ceiling area"), "{err}");
    c.cancel_pending_roof_ridge();
    assert!(!c.roof_ridge_pending(), "cancel recovers");
}

/// Finalize at the CROSS stage: when the start snapped, the cross question
/// is the LAST open location stage, so a length that fits the bbox but not
/// the run from the snapped start is rejected on the typed CROSS distance —
/// with the permitted run — and an achievable cross then lands.
#[test]
fn roof_ridge_finalize_rejection_lands_on_the_cross_when_start_snapped() {
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
    assert!(c.roof_ridge_position_pending(), "cross prompt stays open");
    c.cancel_pending_roof_ridge();
    assert!(!c.roof_ridge_pending());
}

/// A storey switch abandons the in-flight ridge and leaves the mode — its
/// coordinates are in the outgoing storey's space.
#[test]
fn storey_switch_clears_pending_roof_ridge() {
    let mut c = roof_ready_controller();
    c.apply_roof_ridge_stroke_event(&[(3.78, 2.0), (3.82, 5.0), (3.8, 8.0)]);
    assert!(c.roof_ridge_pending());
    c.add_storey().unwrap();
    assert!(!c.roof_point_mode(), "mode reset on switch");
    assert!(!c.roof_ridge_pending(), "pending ridge abandoned");
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

    // Roofs: raised piece at 3.4 over the jut, remainder at 2.4.
    let zs: Vec<f64> = storey
        .roofs
        .regions
        .iter()
        .filter_map(|r| r.flat_z())
        .collect();
    assert!(
        zs.iter().any(|z| (z - 3.4).abs() < 1e-6) && zs.iter().any(|z| (z - 2.4).abs() < 1e-6),
        "roof split into 2.4 + 3.4 pieces: {zs:?}"
    );

    // Walls: classify each marked wall by its base edge and check its top.
    let top_zs = |r: &datatypes::ber::structural::WallRegion| -> Vec<f64> {
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
    let is_riser = |r: &datatypes::ber::structural::WallRegion| {
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
    let risers: Vec<&datatypes::ber::structural::WallRegion> = storey
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
    let span = |r: &datatypes::ber::structural::WallRegion| {
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
    let floor_riser_id = floor_riser.id;
    let ceiling_riser_id = ceiling_riser.id;

    // And the raised tops + risers SURVIVE persist → reload → re-derive (the
    // session step areas are reconstructed from the persisted floors/roofs;
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
    let reloaded: Vec<&datatypes::ber::structural::WallRegion> = storey
        .walls
        .regions
        .iter()
        .filter(|r| is_riser(r))
        .collect();
    assert_eq!(reloaded.len(), 2, "both risers re-derive after reload");
    assert!(
        reloaded.iter().any(|r| r.id == floor_riser_id),
        "floor riser keeps its id across reload"
    );
    assert!(
        reloaded.iter().any(|r| r.id == ceiling_riser_id),
        "ceiling riser keeps its id across reload"
    );
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
    assert!(c.roof_ridge_length_pending(), "length question opens");
    let err = c.assign_roof_ridge_length(25.0).unwrap_err();
    assert!(
        err.contains("No ceiling area is that long"),
        "beyond every bbox → the Length-stage bound: {err}"
    );
    assert!(c.roof_ridge_length_pending(), "pending intact on rejection");
    // …but 1.5 m fits SOME area's bbox, so it passes — the binding check
    // is the location stage's.
    c.assign_roof_ridge_length(1.5)
        .expect("a bbox-fitting length passes the Length stage");

    // Neither drawn position is within snap tolerance (nearest parallel
    // walls sit ≈ 0.46 m / 0.45 m away) → the cross question opens.
    assert!(c.roof_ridge_position_pending(), "cross question opens");
    c.skip_position_dimension(); // cross: the drawn x ≈ 3.75 stands
    assert!(c.roof_ridge_position_pending(), "start question opens");

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
        c.roof_ridge_position_pending(),
        "start prompt stays open on the finalize rejection"
    );

    // A start the length fits from (0.3 m above inner_y leaves a 1.7 m
    // run): accepted…
    c.apply_position_dimension(0.3)
        .expect("a start the length fits from lands");
    assert!(c.roof_ridge_height_pending(), "on to the height question");
    // …and the Height stage must then SUCCEED: finalize acceptance implies
    // the height validation's strict same-region containment.
    c.assign_roof_ridge_height(4.2)
        .expect("no late rejection at the height stage");
    assert!(!c.roof_ridge_pending(), "flow complete");
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.roof_apexes.len(), 2, "both apexes durable");
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
    assert_eq!(
        views[0].id, riser_id,
        "identity stable across the re-derive"
    );

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
/// perimeter wall but doesn't lie on its segment — `riser_views` returns
/// exactly the one interior ceiling riser, and no perimeter wall is ever
/// misclassified as a riser (the wall views stay complete alongside).
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

    let risers = c.riser_views();
    assert_eq!(risers.len(), 1, "exactly the interior ceiling riser");
    assert_eq!(risers[0].kind, RiserKind::Ceiling);
    assert_eq!(risers[0].exposure, WallExposure::Unset, "derived default");
    assert!(
        (risers[0].a.0 - 6.0).abs() < 1e-4 && (risers[0].b.0 - 6.0).abs() < 1e-4,
        "riser base on the step line x = 6: {:?} {:?}",
        risers[0].a,
        risers[0].b
    );
    assert!(
        risers[0].a.1.max(risers[0].b.1) <= 4.0 + 1e-4,
        "…and only on the interior span y ∈ [0, 4], never the wall segment"
    );
    // The perimeter set is untouched: all 6 outline walls still present.
    assert_eq!(c.wall_exposure_views().len(), 6);
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
    c.materialize();
    c.reconstruct_ceiling_step_regions_for(0); // canonicalize, assign parity
    c
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
