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
        c.set_edit_mode(EditTools::Edit);
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
/// Unmarked walls default to `Exposed`.
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

    // Only the MARKED wall derives a region — un-marked walls produce none.
    c.materialize(); // edits defer the derive; bring current_record current
    let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
    assert_eq!(storey.walls.regions.len(), 1, "only the marked wall");
    let _ = walls;
    let w0 = &storey.walls.regions[0];
    assert_eq!(w0.exposure, WallExposure::SemiExposed);
    assert_eq!(w0.polygon.exterior.len(), 4, "vertical plane = 4 vertices");
    assert!(w0.polygon.exterior.iter().any(|q| q.z == 0.0));
    assert!(w0
        .polygon
        .exterior
        .iter()
        .any(|q| (q.z - DEFAULT_CEILING_HEIGHT_M).abs() < 1e-9));
}

/// A section over PART of a wall derives several regions: the marked range
/// plus the un-marked remainder as Exposed, sub-segment lengths summing to
/// the wall length.
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
    // The wall is materialized into 3 explicit sections: Exposed, Semi,
    // Exposed (the leftovers become their own sections).
    let secs = c
        .document
        .wall_sections
        .get(&vid0)
        .expect("wall0 has sections");
    assert_eq!(secs.len(), 3);
    assert_eq!(secs[0].exposure, WallExposure::Exposed);
    assert_eq!(secs[1].exposure, WallExposure::SemiExposed);
    assert_eq!(secs[2].exposure, WallExposure::Exposed);

    // wall0 is derived first → its 3 sub-regions are regions[0..3].
    c.materialize(); // edits defer the derive; bring current_record current
    let r = &c.current_record.as_ref().unwrap().data.storeys[0]
        .walls
        .regions;
    assert!(r.len() >= 3);
    assert_eq!(r[0].exposure, WallExposure::Exposed);
    assert_eq!(r[1].exposure, WallExposure::SemiExposed);
    assert_eq!(r[2].exposure, WallExposure::Exposed);
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
    assert!(c.step_pick_pending(), "now the amount field shows");
    assert!(
        c.pending_step_polygon().len() >= 4,
        "dimensioned ring available: {:?}",
        c.pending_step_polygon()
    );

    // Assign the level → stored region, back to the outline.
    c.assign_step_level_to_pending(-0.3);
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
    assert!(c.step_pick_pending(), "carved area awaits its amount");

    c.assign_step_level_to_pending(0.005);
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
    assert!(c.step_pick_pending(), "carved area awaits its amount");
    c.assign_step_level_to_pending(-0.3);
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
    c.set_wall_exposure(WallExposure::SemiExposed); // → 3 sections

    c.materialize(); // edits defer the derive; bring current_record current
    let storey = c.current_record.as_ref().unwrap().data.storeys[0].clone();
    c.document.wall_sections.clear();
    c.load_storey_into_editor(Some(&storey));
    assert_eq!(
        c.document.wall_sections.len(),
        1,
        "one split wall reconstructed"
    );
    let secs = c.document.wall_sections.values().next().unwrap();
    assert_eq!(secs.len(), 3);
    assert_eq!(secs[0].exposure, WallExposure::Exposed);
    assert_eq!(secs[1].exposure, WallExposure::SemiExposed);
    assert_eq!(secs[2].exposure, WallExposure::Exposed);
    assert!(
        (secs[1].start_m - len * 0.2).abs() < 0.02,
        "{}",
        secs[1].start_m
    );
    assert!(
        (secs[1].end_m - len * 0.5).abs() < 0.02,
        "{}",
        secs[1].end_m
    );
}

/// "Set remaining → Exposed" fills only walls with no sections yet, leaving
/// already-marked walls (semi/non) untouched.
#[test]
fn set_all_remaining_fills_only_unset_walls() {
    let mut c = dimensioned_square_controller();
    let walls = c.document.outline.floor_plan.perimeter_walls();
    let (vid0, a0, b0) = walls[0];
    let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
    c.set_exposure_mode(true);
    c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
    c.set_wall_exposure(WallExposure::SemiExposed);

    c.set_all_remaining_exposed();
    assert_eq!(
        c.document.wall_sections.len(),
        walls.len(),
        "every wall now set"
    );
    assert_eq!(
        c.document.wall_sections.get(&vid0).map(|s| s[0].exposure),
        Some(WallExposure::SemiExposed),
        "the marked wall keeps its type"
    );
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
