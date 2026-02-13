// Example: Working with geometric constraints

use floorplan_geometry::*;

fn main() -> Result<(), Error> {
    println!("=== Geometric Constraints Example ===\n");
    
    // Create a simple L-shaped floor plan
    let points = vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 5.0),
        Point::new(5.0, 5.0),
        Point::new(5.0, 10.0),
        Point::new(0.0, 10.0),
    ];
    
    let stroke = Stroke::new(points);
    let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;
    
    println!("Created L-shaped floor plan with {} vertices\n", plan.polygon.vertex_count());
    
    // 1. Add parallel constraints
    println!("1. Adding parallel constraints...");
    plan.constraints.add_parallel(0, 2)?;
    println!("   Made edges 0 and 2 parallel");
    
    plan.constraints.add_parallel(1, 3)?;
    println!("   Made edges 1 and 3 parallel");
    
    // 2. Add fixed length constraints
    println!("\n2. Adding fixed length constraints...");
    plan.constraints.add_fixed_length(0, 10.0)?;
    println!("   Fixed edge 0 to 10.0 units");
    
    plan.constraints.add_fixed_length(1, 5.0)?;
    println!("   Fixed edge 1 to 5.0 units");
    
    // 3. Check collinear suggestions
    println!("\n3. Checking for collinear suggestions...");
    let suggestions = plan.suggest_collinear_constraints(0.5);
    
    if suggestions.is_empty() {
        println!("   No collinear edges found");
    } else {
        for suggestion in &suggestions {
            println!("   {}", suggestion.description);
            println!("      Confidence: {:.2}", suggestion.confidence);
            
            if suggestion.confidence > 0.7 {
                println!("      -> Adding constraint");
                plan.add_collinear_constraint(suggestion.edge1, suggestion.edge2)?;
            }
        }
    }
    
    // 4. Add a perpendicular constraint
    println!("\n4. Adding perpendicular constraint...");
    plan.constraints.add_perpendicular(0, 1)?;
    println!("   Made edges 0 and 1 perpendicular");
    
    // 5. Show all active constraints
    println!("\n5. Active constraints:");
    for (i, constraint) in plan.constraints.constraints.iter().enumerate() {
        match constraint {
            constraints::Constraint::Parallel { edge1, edge2 } => {
                println!("   {}: Edges {} and {} are parallel", i, edge1, edge2);
            }
            constraints::Constraint::Perpendicular { edge1, edge2 } => {
                println!("   {}: Edges {} and {} are perpendicular", i, edge1, edge2);
            }
            constraints::Constraint::Collinear { edge1, edge2 } => {
                println!("   {}: Edges {} and {} are collinear", i, edge1, edge2);
            }
            constraints::Constraint::FixedLength { edge, length } => {
                println!("   {}: Edge {} has length {:.2}", i, edge, length);
            }
            constraints::Constraint::EqualLength { edge1, edge2 } => {
                println!("   {}: Edges {} and {} have equal length", i, edge1, edge2);
            }
            constraints::Constraint::FixedVertex { vertex, position } => {
                println!("   {}: Vertex {} fixed at ({:.2}, {:.2})",
                    i, vertex, position.x, position.y);
            }
        }
    }
    
    // 6. Verify edge properties
    println!("\n6. Edge properties after constraints:");
    for (i, edge) in plan.polygon.edges().iter().enumerate() {
        println!("   Edge {}: length = {:.2}, angle = {:.2}°",
            i,
            edge.length(),
            edge.angle().to_degrees()
        );
    }
    
    // 7. Test constraint removal
    println!("\n7. Removing a constraint...");
    println!("   Removing constraint 0");
    plan.constraints.remove_constraint(0);
    println!("   Active constraints: {}", plan.constraints.constraints.len());
    
    // 8. Add equal length constraint
    println!("\n8. Adding equal length constraint...");
    plan.constraints.constraints.push(
        constraints::Constraint::EqualLength { edge1: 0, edge2: 4 }
    );
    plan.apply_constraints()?;
    
    let edge0 = plan.polygon.edge(0).unwrap();
    let edge4 = plan.polygon.edge(4).unwrap();
    println!("   Edge 0 length: {:.2}", edge0.length());
    println!("   Edge 4 length: {:.2}", edge4.length());
    println!("   Difference: {:.2}", (edge0.length() - edge4.length()).abs());
    
    println!("\n=== Example Complete ===");
    
    Ok(())
}
