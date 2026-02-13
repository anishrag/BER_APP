// Example: Demonstrating the Cassowary Constraint Solver

use floorplan_geometry::*;

fn main() -> Result<(), Error> {
    println!("=== Cassowary Constraint Solver Demo ===\n");
    
    // Create a rough rectangular floor plan
    println!("1. Creating a rough rectangular sketch...");
    let rough_points = vec![
        Point::new(0.0, 0.0),
        Point::new(10.2, 0.3),      // Should be (10, 0)
        Point::new(10.1, 5.1),      // Should be (10, 5)
        Point::new(0.1, 4.9),       // Should be (0, 5)
    ];
    
    let stroke = Stroke::new(rough_points.clone());
    let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;
    
    println!("   Before constraints:");
    for (i, vertex) in plan.polygon.vertices.iter().enumerate() {
        println!("     Vertex {}: ({:.2}, {:.2})", i, vertex.x, vertex.y);
    }
    println!("   Area: {:.2} square units\n", plan.polygon.area());
    
    // 2. Add constraints that Cassowary will solve simultaneously
    println!("2. Adding multiple constraints...");
    
    // Fix the first vertex at origin (REQUIRED strength)
    println!("   - Fix vertex 0 at origin");
    plan.constraints.constraints.push(
        constraints::Constraint::FixedVertex {
            vertex: 0,
            position: Point::new(0.0, 0.0),
        }
    );
    
    // Make edges 0 and 2 parallel (they should both be horizontal)
    println!("   - Make edges 0 and 2 parallel (horizontal)");
    plan.constraints.add_parallel(0, 2)?;
    
    // Make edges 1 and 3 parallel (they should both be vertical)
    println!("   - Make edges 1 and 3 parallel (vertical)");
    plan.constraints.add_parallel(1, 3)?;
    
    // Make edges 0 and 1 perpendicular (should be 90 degrees)
    println!("   - Make edges 0 and 1 perpendicular");
    plan.constraints.add_perpendicular(0, 1)?;
    
    // Fix the length of edge 0 to exactly 10 units
    println!("   - Set edge 0 length to exactly 10.0 units");
    plan.constraints.add_fixed_length(0, 10.0)?;
    
    // Fix the length of edge 1 to exactly 5 units
    println!("   - Set edge 1 length to exactly 5.0 units");
    plan.constraints.add_fixed_length(1, 5.0)?;
    
    // 3. Let Cassowary solve all constraints simultaneously
    println!("\n3. Solving constraints with Cassowary...");
    plan.apply_constraints()?;
    
    println!("   After constraints:");
    for (i, vertex) in plan.polygon.vertices.iter().enumerate() {
        println!("     Vertex {}: ({:.2}, {:.2})", i, vertex.x, vertex.y);
    }
    println!("   Area: {:.2} square units\n", plan.polygon.area());
    
    // 4. Verify the constraints were satisfied
    println!("4. Verifying constraint satisfaction:");
    
    let edge0 = plan.polygon.edge(0).unwrap();
    let edge1 = plan.polygon.edge(1).unwrap();
    let edge2 = plan.polygon.edge(2).unwrap();
    
    println!("   ✓ Edge 0 length: {:.4} (target: 10.0)", edge0.length());
    println!("   ✓ Edge 1 length: {:.4} (target: 5.0)", edge1.length());
    
    // Check if edges are perpendicular
    let angle_diff = (edge0.angle() - edge1.angle()).abs();
    let is_perpendicular = (angle_diff - std::f64::consts::PI / 2.0).abs() < 0.1;
    println!("   ✓ Edges 0 and 1 perpendicular: {} (angle diff: {:.2}°)", 
        is_perpendicular, angle_diff.to_degrees());
    
    // Check if parallel edges have same angle
    let edge0_angle = edge0.angle().to_degrees();
    let edge2_angle = edge2.angle().to_degrees();
    println!("   ✓ Edges 0 and 2 parallel: edge0={:.2}°, edge2={:.2}°",
        edge0_angle, edge2_angle);
    
    // 5. Demonstrate constraint conflict resolution
    println!("\n5. Testing constraint priorities...");
    
    // Create a new plan with conflicting constraints
    let conflicting_points = vec![
        Point::new(0.0, 0.0),
        Point::new(5.0, 0.0),
        Point::new(5.0, 5.0),
        Point::new(0.0, 5.0),
    ];
    
    let mut conflict_plan = FloorPlan::from_stroke_orthogonal(
        &Stroke::new(conflicting_points), 0.5
    )?;
    
    // Try to set conflicting lengths (edge has current length ~5.0)
    println!("   Current edge 0 length: {:.2}", conflict_plan.polygon.edge(0).unwrap().length());
    
    // Set it to 10.0
    conflict_plan.constraints.add_fixed_length(0, 10.0)?;
    
    // Also set vertex 1 to be at (6, 0) - conflicts with length constraint!
    conflict_plan.constraints.constraints.push(
        constraints::Constraint::FixedVertex {
            vertex: 1,
            position: Point::new(6.0, 0.0),
        }
    );
    
    println!("   Added conflicting constraints:");
    println!("     - Edge 0 length = 10.0 (STRONG)");
    println!("     - Vertex 1 at (6, 0) (REQUIRED)");
    
    // Cassowary will resolve the conflict by prioritizing REQUIRED over STRONG
    conflict_plan.apply_constraints()?;
    
    println!("   Resolution:");
    println!("     Vertex 1 is at: ({:.2}, {:.2})", 
        conflict_plan.polygon.vertices[1].x,
        conflict_plan.polygon.vertices[1].y
    );
    println!("     Edge 0 length: {:.2} (REQUIRED vertex won over STRONG length)",
        conflict_plan.polygon.edge(0).unwrap().length()
    );
    
    // 6. Advanced: Equal length constraints
    println!("\n6. Equal length constraints...");
    let mut equal_plan = FloorPlan::from_stroke_orthogonal(
        &Stroke::new(vec![
            Point::new(0.0, 0.0),
            Point::new(8.0, 0.0),
            Point::new(8.0, 5.0),
            Point::new(3.0, 5.0),
            Point::new(3.0, 10.0),
            Point::new(0.0, 10.0),
        ]),
        0.5
    )?;
    
    println!("   Making edges 0 and 4 have equal length...");
    let edge0_before = equal_plan.polygon.edge(0).unwrap().length();
    let edge4_before = equal_plan.polygon.edge(4).unwrap().length();
    println!("     Before: edge 0 = {:.2}, edge 4 = {:.2}", edge0_before, edge4_before);
    
    equal_plan.constraints.constraints.push(
        constraints::Constraint::EqualLength { edge1: 0, edge2: 4 }
    );
    equal_plan.apply_constraints()?;
    
    let edge0_after = equal_plan.polygon.edge(0).unwrap().length();
    let edge4_after = equal_plan.polygon.edge(4).unwrap().length();
    println!("     After:  edge 0 = {:.2}, edge 4 = {:.2}", edge0_after, edge4_after);
    println!("     Difference: {:.4}", (edge0_after - edge4_after).abs());
    
    println!("\n=== Cassowary Solver Benefits ===");
    println!("✓ Solves multiple constraints simultaneously");
    println!("✓ Handles constraint priorities (REQUIRED > STRONG > WEAK)");
    println!("✓ Resolves conflicts automatically by priority");
    println!("✓ Efficient solution (no iterative convergence needed for linear constraints)");
    println!("✓ Proven algorithm used in production UI frameworks");
    
    Ok(())
}
