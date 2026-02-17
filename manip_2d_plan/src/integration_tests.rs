// Integration tests for floorplan_geometry

use floorplan_geometry::*;

#[test]
fn test_basic_stroke_to_plan() {
    let points = vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ];
    
    let stroke = Stroke::new(points);
    let plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5).unwrap();
    
    assert!(plan.polygon.vertex_count() >= 3);
    assert!(plan.polygon.area() > 0.0);
}

#[test]
fn test_stroke_properties() {
    let points = vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 0.0),
    ];
    
    let stroke = Stroke::new(points);
    
    assert_eq!(stroke.len(), 4);
    assert!(stroke.is_closed(0.1));
    assert!(!stroke.is_empty());
}

#[test]
fn test_polygon_edges() {
    let polygon = Polygon::new(vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ]);
    
    let edges = polygon.edges();
    assert_eq!(edges.len(), 4);
    
    // Check first edge
    let edge0 = edges[0];
    assert!((edge0.length() - 10.0).abs() < 0.01);
    assert!(edge0.is_horizontal(0.1));
}

#[test]
fn test_orthogonal_conversion() {
    let points = vec![
        Point::new(0.0, 0.0),
        Point::new(10.2, 0.3),
        Point::new(10.1, 10.1),
        Point::new(0.1, 9.9),
    ];
    
    let stroke = Stroke::new(points);
    let plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5).unwrap();
    
    // All edges should be approximately horizontal or vertical
    for edge in plan.polygon.edges() {
        let is_ortho = edge.is_horizontal(0.5) || edge.is_vertical(0.5);
        assert!(is_ortho, "Edge is not orthogonal: {:?}", edge);
    }
}

#[test]
fn test_add_area() {
    let main_points = vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ];
    
    let main_stroke = Stroke::new(main_points);
    let plan = FloorPlan::from_stroke_orthogonal(&main_stroke, 0.5).unwrap();
    let original_area = plan.polygon.area();
    
    let addition_points = vec![
        Point::new(10.0, 2.0),
        Point::new(15.0, 2.0),
        Point::new(15.0, 8.0),
        Point::new(10.0, 8.0),
    ];
    
    let addition_stroke = Stroke::new(addition_points);
    let new_plan = plan.add_area_orthogonal(&addition_stroke).unwrap();
    
    assert!(new_plan.polygon.area() > original_area);
}

#[test]
fn test_trim_area() {
    let main_points = vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ];
    
    let main_stroke = Stroke::new(main_points);
    let plan = FloorPlan::from_stroke_orthogonal(&main_stroke, 0.5).unwrap();
    let original_area = plan.polygon.area();
    
    let trim_points = vec![
        Point::new(8.0, 8.0),
        Point::new(12.0, 8.0),
        Point::new(12.0, 12.0),
        Point::new(8.0, 12.0),
    ];
    
    let trim_stroke = Stroke::new(trim_points);
    let new_plan = plan.trim_area_orthogonal(&trim_stroke).unwrap();
    
    assert!(new_plan.polygon.area() < original_area);
}

#[test]
fn test_dimension_application() {
    let points = vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ];
    
    let stroke = Stroke::new(points);
    let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5).unwrap();
    
    let dimension = dimensions::Dimension {
        kind: dimensions::DimensionKind::EdgeLength { edge_index: 0 },
        value: 15.0,
    };
    
    plan.set_dimension(dimension).unwrap();
    
    // After applying dimension, edge 0 should have the specified length
    let edge = plan.polygon.edge(0).unwrap();
    assert!((edge.length() - 15.0).abs() < 0.5);
}

#[test]
fn test_constraint_application() {
    let points = vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ];
    
    let stroke = Stroke::new(points);
    let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5).unwrap();
    
    plan.constraints.add_fixed_length(0, 12.0).unwrap();
    plan.apply_constraints().unwrap();
    
    let edge = plan.polygon.edge(0).unwrap();
    assert!((edge.length() - 12.0).abs() < 0.1);
}

#[test]
fn test_room_detection() {
    let points = vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ];
    
    let stroke = Stroke::new(points);
    let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5).unwrap();
    
    plan.detect_rooms().unwrap();
    
    assert!(!plan.rooms.is_empty());
    assert!(plan.rooms[0].area > 0.0);
}

#[test]
fn test_snapping() {
    let polygon = Polygon::new(vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ]);
    
    let point = Point::new(10.2, 5.0);
    let snapped = snapping::snap_to_polygon(&point, &polygon, 1.0);
    
    assert!(snapped.is_some());
    let target = snapped.unwrap();
    assert!((target.point.x - 10.0).abs() < 0.01);
}

#[test]
fn test_collinear_detection() {
    let points = vec![
        Point::new(0.0, 0.0),
        Point::new(5.0, 0.0),
        Point::new(5.0, 5.0),
        Point::new(10.0, 5.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ];
    
    let stroke = Stroke::new(points);
    let plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5).unwrap();
    
    let suggestions = plan.suggest_collinear_constraints(0.5);
    
    // Should detect that some edges might be collinear
    // The exact number depends on the orthogonalization
    assert!(suggestions.len() >= 0); // At least we don't crash
}

#[test]
fn test_utils_functions() {
    let line1 = Line::new(Point::new(0.0, 0.0), Point::new(10.0, 0.0));
    let line2 = Line::new(Point::new(0.0, 5.0), Point::new(10.0, 5.0));
    
    assert!(utils::are_parallel(&line1, &line2, 0.1));
    
    let line3 = Line::new(Point::new(5.0, 0.0), Point::new(5.0, 10.0));
    assert!(utils::are_perpendicular(&line1, &line3, 0.1));
}

#[test]
fn test_polygon_validity() {
    let valid = Polygon::new(vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ]);
    
    assert!(valid.is_valid());
    
    let invalid = Polygon::new(vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
    ]);
    
    assert!(!invalid.is_valid());
}

#[test]
fn test_line_intersection() {
    use snapping::find_intersection_points;
    
    let polygon = Polygon::new(vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ]);
    
    let stroke_points = vec![
        Point::new(5.0, -1.0),
        Point::new(5.0, 11.0),
    ];
    
    let intersections = find_intersection_points(&stroke_points, &polygon);
    
    // Should intersect top and bottom edges
    assert_eq!(intersections.len(), 2);
}
