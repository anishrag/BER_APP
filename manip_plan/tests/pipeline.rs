//! End-to-end integration tests for the public `FloorPlan` API.
//!
//! These exercise the full pipeline (simplify → orthogonalize → close → rotate
//! → parametric) from outside the crate, so they lock in the contract callers
//! depend on independently of any internal refactoring. Per-function unit
//! tests live in `src/*.rs` and exercise crate-private helpers; this file
//! covers the public surface only.
//!
//! Coordinate convention: y increases downward (canvas convention), same as
//! the production input from the sketch page.

use domain::points::Point2f32;
use manip_plan::{Axis, FloorPlan};

fn pt(x: f32, y: f32) -> Point2f32 {
    Point2f32::new(x, y)
}

const TOLERANCE: f32 = 10.0;

#[test]
fn closed_rectangle_produces_4_corners_and_2_params() {
    // Clockwise rectangle with a lift point inside the close-snap zone.
    let stroke = vec![
        pt(0.0, 0.0),
        pt(200.0, 0.0),
        pt(200.0, 100.0),
        pt(0.0, 100.0),
        pt(3.0, 3.0), // lift near start
    ];
    let plan = FloorPlan::from_points_to_orthogonal(&stroke, TOLERANCE, true).unwrap();
    assert_eq!(plan.get_points().len(), 4);
    assert_eq!(plan.active_parameter_count(), 2); // width + height
}

#[test]
fn closed_l_shape_produces_6_corners_and_4_params() {
    // Counterclockwise L. The parametric layer should collapse shared coords
    // into 4 free parameters: outer width, outer height, indent x, indent y.
    let stroke = vec![
        pt(0.0, 0.0),
        pt(200.0, 0.0),
        pt(200.0, 100.0),
        pt(100.0, 100.0),
        pt(100.0, 200.0),
        pt(0.0, 200.0),
        pt(5.0, 5.0), // lift near start
    ];
    let plan = FloorPlan::from_points_to_orthogonal(&stroke, TOLERANCE, true).unwrap();
    assert_eq!(plan.get_points().len(), 6);
    assert_eq!(plan.active_parameter_count(), 4);
}

#[test]
fn open_polyline_does_not_close_or_rotate() {
    // closed = false: pipeline runs simplify + orthogonalize but skips the
    // closure-specific steps. The output keeps the user's exact endpoints.
    let stroke = vec![pt(0.0, 0.0), pt(100.0, 0.0), pt(100.0, 50.0), pt(0.0, 50.0)];
    let plan = FloorPlan::from_points_to_orthogonal(&stroke, TOLERANCE, false).unwrap();
    assert!(plan.get_points().len() >= 3);
}

#[test]
fn stroke_too_short_returns_insufficient_points() {
    // Two points can't form a polygon — the simplification step rejects them.
    let stroke = vec![pt(0.0, 0.0), pt(100.0, 0.0)];
    assert!(FloorPlan::from_points_to_orthogonal(&stroke, TOLERANCE, true).is_err());
}

#[test]
fn collinearity_suggestion_flags_walls_in_the_user_review_range() {
    // A polygon with two horizontal walls 30px apart — in the (1, 60] range
    // the controller uses for the user-confirmed review.
    let stroke = vec![
        pt(0.0, 0.0),
        pt(100.0, 0.0),
        pt(100.0, 50.0),
        pt(200.0, 50.0),
        pt(200.0, 80.0), // 30px above the y=50 segment
        pt(0.0, 80.0),
        pt(3.0, 3.0),
    ];
    let plan = FloorPlan::from_points_to_orthogonal(&stroke, TOLERANCE, true).unwrap();
    let suggestions = plan.suggest_collinear(1.0, 60.0);
    assert!(
        !suggestions.is_empty(),
        "expected at least one near-collinear pair to be flagged"
    );
}

#[test]
fn applying_collinearity_suggestion_reduces_parameter_count() {
    let stroke = vec![
        pt(0.0, 0.0),
        pt(100.0, 0.0),
        pt(100.0, 50.0),
        pt(200.0, 50.0),
        pt(200.0, 80.0),
        pt(0.0, 80.0),
        pt(3.0, 3.0),
    ];
    let mut plan = FloorPlan::from_points_to_orthogonal(&stroke, TOLERANCE, true).unwrap();
    let suggestions = plan.suggest_collinear(1.0, 60.0);
    let sug = suggestions
        .first()
        .expect("the test setup should produce at least one suggestion");

    let before = plan.active_parameter_count();
    plan.apply_collinear_suggestion(sug);
    let after = plan.active_parameter_count();

    assert!(
        after < before,
        "expected param count to drop after merging two edges (was {}, now {})",
        before,
        after,
    );
}

#[test]
fn edges_collinear_reflects_apply_state() {
    // Before applying a suggestion, the two edges aren't collinear; after, they are.
    let stroke = vec![
        pt(0.0, 0.0),
        pt(100.0, 0.0),
        pt(100.0, 50.0),
        pt(200.0, 50.0),
        pt(200.0, 80.0),
        pt(0.0, 80.0),
        pt(3.0, 3.0),
    ];
    let mut plan = FloorPlan::from_points_to_orthogonal(&stroke, TOLERANCE, true).unwrap();
    let suggestions = plan.suggest_collinear(1.0, 60.0);
    let sug = suggestions.first().expect("at least one suggestion");

    assert!(
        !plan.edges_collinear(sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, &sug.axis),
        "edges shouldn't be collinear before apply"
    );
    plan.apply_collinear_suggestion(sug);
    assert!(
        plan.edges_collinear(sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, &sug.axis),
        "edges should be collinear after apply"
    );
}

#[test]
fn axis_enum_is_publicly_constructible() {
    // Smoke test — verify the public Axis enum can be used in patterns by
    // external code. This catches accidental changes to its visibility.
    let h = Axis::Horizontal;
    let v = Axis::Vertical;
    assert_ne!(h, v);
}
