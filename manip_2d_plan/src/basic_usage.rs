// Example: Basic floor plan creation and manipulation

use floorplan_geometry::*;

fn main() -> Result<(), Error> {
    println!("=== Floorplan Geometry Example ===\n");
    
    // 1. Create a hand-drawn stroke (simulating user input)
    println!("1. Creating a hand-drawn stroke...");
    let points = vec![
        Point::new(0.0, 0.0),
        Point::new(0.5, 0.1),
        Point::new(1.0, 0.0),
        Point::new(2.5, 0.2),
        Point::new(5.0, 0.1),
        Point::new(10.2, 0.3),
        Point::new(10.1, 2.0),
        Point::new(10.3, 5.0),
        Point::new(10.1, 8.0),
        Point::new(10.1, 10.1),
        Point::new(8.0, 9.9),
        Point::new(5.0, 10.2),
        Point::new(2.0, 10.0),
        Point::new(0.1, 9.9),
        Point::new(0.2, 7.0),
        Point::new(0.0, 5.0),
        Point::new(0.1, 2.0),
    ];
    
    let stroke = Stroke::new(points);
    println!("   Stroke has {} points", stroke.len());
    println!("   Total length: {:.2} units", stroke.total_length());
    
    // 2. Simplify to orthogonal polygon
    println!("\n2. Simplifying to orthogonal polygon...");
    let tolerance = 0.5;
    let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, tolerance)?;
    
    println!("   Simplified to {} vertices", plan.polygon.vertex_count());
    println!("   Area: {:.2} square units", plan.polygon.area());
    
    // 3. Display edges
    println!("\n3. Floor plan edges:");
    for (i, edge) in plan.polygon.edges().iter().enumerate() {
        println!("   Edge {}: ({:.2}, {:.2}) -> ({:.2}, {:.2}), length: {:.2}",
            i,
            edge.start.x, edge.start.y,
            edge.end.x, edge.end.y,
            edge.length()
        );
    }
    
    // 4. Add a dimension constraint
    println!("\n4. Adding dimension constraint...");
    let dimension = dimensions::Dimension {
        kind: dimensions::DimensionKind::EdgeLength { edge_index: 0 },
        value: 12.0,
    };
    plan.set_dimension(dimension)?;
    println!("   Set edge 0 to 12.0 units");
    
    // 5. Check for collinear suggestions
    println!("\n5. Checking for collinear edges...");
    let suggestions = plan.suggest_collinear_constraints(0.5);
    if suggestions.is_empty() {
        println!("   No collinear edges detected");
    } else {
        for suggestion in &suggestions {
            println!("   {} (confidence: {:.2})",
                suggestion.description,
                suggestion.confidence
            );
        }
    }
    
    // 6. Add an area
    println!("\n6. Adding an extension to the plan...");
    let extension_points = vec![
        Point::new(10.0, 5.0),
        Point::new(15.0, 5.0),
        Point::new(15.0, 8.0),
        Point::new(10.0, 8.0),
    ];
    let extension_stroke = Stroke::new(extension_points);
    plan = plan.add_area_orthogonal(&extension_stroke)?;
    println!("   New area: {:.2} square units", plan.polygon.area());
    
    // 7. Get required dimensions
    println!("\n7. Required dimensions for full constraint:");
    let required = plan.required_dimensions();
    for (i, dim) in required.iter().take(5).enumerate() {
        println!("   {}: {} (current: {:.2}, priority: {:?})",
            i + 1,
            dim.description,
            dim.current_value,
            dim.priority
        );
    }
    
    // 8. Shift a wall
    println!("\n8. Shifting a wall...");
    let original_area = plan.polygon.area();
    plan.shift_wall(1, 1.5)?;
    let new_area = plan.polygon.area();
    println!("   Area changed from {:.2} to {:.2}", original_area, new_area);
    
    // 9. Detect rooms
    println!("\n9. Detecting rooms...");
    plan.detect_rooms()?;
    println!("   Found {} room(s)", plan.rooms.len());
    for (i, room) in plan.rooms.iter().enumerate() {
        println!("   Room {}: Area = {:.2} at ({:.2}, {:.2})",
            i + 1,
            room.area,
            room.center.x,
            room.center.y
        );
        
        let suggestions = rooms::suggest_room_type(room);
        println!("      Suggested types: {:?}", suggestions);
    }
    
    // 10. Final statistics
    println!("\n10. Final floor plan statistics:");
    println!("   Vertices: {}", plan.polygon.vertex_count());
    println!("   Area: {:.2} square units", plan.polygon.area());
    println!("   Perimeter: {:.2} units", utils::perimeter(&plan.polygon));
    println!("   Constraints: {}", plan.constraints.constraints.len());
    
    Ok(())
}
