// Example: Proper workflow for creating a fully-constrained floor plan

use floorplan_geometry::*;

fn main() -> Result<(), Error> {
    println!("=== Floor Plan Constraint Workflow ===\n");
    println!("This example shows the recommended workflow:");
    println!("1. Create orthogonal plan (auto-adds horizontal/vertical constraints)");
    println!("2. User confirms collinear edges");
    println!("3. Calculate minimum required dimensions");
    println!("4. User provides dimension values");
    println!("5. Plan is fully constrained!\n");
    
    // ========== STEP 1: Create Orthogonal Plan ==========
    println!("STEP 1: Create orthogonal plan from sketch");
    println!("-------------------------------------------");
    
    let rough_sketch = vec![
        Point::new(0.0, 0.0),
        Point::new(0.2, 0.1),
        Point::new(2.0, 0.0),
        Point::new(5.1, 0.2),
        Point::new(10.3, 0.1),
        Point::new(10.2, 2.5),
        Point::new(10.1, 5.0),
        Point::new(10.0, 7.5),
        Point::new(10.2, 10.1),
        Point::new(7.9, 10.0),
        Point::new(5.0, 10.2),
        Point::new(2.1, 9.9),
        Point::new(0.1, 10.0),
        Point::new(0.0, 7.8),
        Point::new(0.2, 5.0),
        Point::new(0.1, 2.3),
    ];
    
    let stroke = Stroke::new(rough_sketch);
    let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;
    
    println!("✓ Created orthogonal plan with {} vertices", plan.polygon.vertex_count());
    println!("✓ Auto-generated {} orientation constraints:", plan.constraints.constraints.len());
    
    for (i, constraint) in plan.constraints.get_orientation_constraints().iter().enumerate() {
        match constraint {
            constraints::Constraint::Horizontal { edge } => {
                println!("    Constraint {}: Edge {} is HORIZONTAL", i, edge);
            }
            constraints::Constraint::Vertical { edge } => {
                println!("    Constraint {}: Edge {} is VERTICAL", i, edge);
            }
            _ => {}
        }
    }
    
    // ========== STEP 2: Suggest and Confirm Collinear Edges ==========
    println!("\nSTEP 2: Check for collinear edges");
    println!("-------------------------------------------");
    
    let suggestions = plan.suggest_collinear_constraints(0.5);
    
    if suggestions.is_empty() {
        println!("✓ No collinear edges detected");
    } else {
        println!("Found {} potential collinear edge pairs:", suggestions.len());
        for (i, suggestion) in suggestions.iter().enumerate() {
            println!("  Suggestion {}: {}", i + 1, suggestion.description);
            println!("    Confidence: {:.1}%", suggestion.confidence * 100.0);
            
            // In a real app, you'd ask the user to confirm
            // For this example, we'll auto-accept high-confidence suggestions
            if suggestion.confidence > 0.8 {
                println!("    → Auto-accepting (high confidence)");
                plan.add_collinear_constraint(suggestion.edge1, suggestion.edge2)?;
            } else {
                println!("    → Skipping (low confidence)");
            }
        }
        
        println!("\n✓ Collinear constraints added: {}", 
            plan.constraints.get_collinear_constraints().len());
    }
    
    // ========== STEP 3: Calculate Required Dimensions ==========
    println!("\nSTEP 3: Calculate minimum required dimensions");
    println!("-------------------------------------------");
    
    let required_dims = plan.calculate_required_dimensions();
    
    println!("System needs {} dimensions to be fully constrained", 
        required_dims.iter().filter(|d| d.priority == dimensions::DimensionPriority::Critical).count()
    );
    println!("\nDimensions needed:");
    
    for (i, dim_req) in required_dims.iter().enumerate() {
        match &dim_req.kind {
            dimensions::DimensionKind::EdgeLength { edge_index } => {
                let edge = plan.polygon.edge(*edge_index).unwrap();
                let orientation = if edge.is_horizontal(0.1) { "horizontal" } else { "vertical" };
                
                println!("  {}. Edge {} ({}) - Current: {:.2} units [{:?}]",
                    i + 1,
                    edge_index,
                    orientation,
                    dim_req.current_value,
                    dim_req.priority
                );
            }
            dimensions::DimensionKind::OverallWidth => {
                println!("  {}. Overall width - Current: {:.2} units [{:?}]",
                    i + 1,
                    dim_req.current_value,
                    dim_req.priority
                );
            }
            dimensions::DimensionKind::OverallHeight => {
                println!("  {}. Overall height - Current: {:.2} units [{:?}]",
                    i + 1,
                    dim_req.current_value,
                    dim_req.priority
                );
            }
            _ => {}
        }
    }
    
    // ========== STEP 4: User Provides Dimensions ==========
    println!("\nSTEP 4: Apply user-provided dimensions");
    println!("-------------------------------------------");
    
    // In a real app, you'd prompt the user for each critical dimension
    // For this example, we'll provide some values
    
    println!("User provides dimensions:");
    
    // Get the first few critical dimensions and apply them
    let critical_dims: Vec<_> = required_dims.iter()
        .filter(|d| d.priority == dimensions::DimensionPriority::Critical)
        .take(3)
        .collect();
    
    for (i, dim_req) in critical_dims.iter().enumerate() {
        // Simulate user input (in real app, this would be UI input)
        let user_value = match i {
            0 => 10.0,  // First edge: 10 units
            1 => 8.0,   // Second edge: 8 units
            2 => 6.0,   // Third edge: 6 units
            _ => dim_req.current_value,
        };
        
        let dimension = dimensions::Dimension {
            kind: dim_req.kind.clone(),
            value: user_value,
        };
        
        plan.set_dimension(dimension)?;
        
        match &dim_req.kind {
            dimensions::DimensionKind::EdgeLength { edge_index } => {
                println!("  ✓ Set edge {} length to {:.2} units", edge_index, user_value);
            }
            _ => {
                println!("  ✓ Set dimension to {:.2} units", user_value);
            }
        }
    }
    
    // ========== STEP 5: Verify Full Constraint ==========
    println!("\nSTEP 5: Verify plan is fully constrained");
    println!("-------------------------------------------");
    
    println!("Total constraints applied:");
    println!("  • Orientation (H/V): {}", plan.constraints.get_orientation_constraints().len());
    println!("  • Collinear: {}", plan.constraints.get_collinear_constraints().len());
    println!("  • Length: {}", plan.constraints.get_length_constraints().len());
    println!("  • Total: {}", plan.constraints.constraints.len());
    
    println!("\nFinal plan properties:");
    println!("  • Vertices: {}", plan.polygon.vertex_count());
    println!("  • Area: {:.2} square units", plan.polygon.area());
    println!("  • Perimeter: {:.2} units", utils::perimeter(&plan.polygon));
    
    println!("\nVertex positions:");
    for (i, vertex) in plan.polygon.vertices.iter().enumerate() {
        println!("  Vertex {}: ({:.2}, {:.2})", i, vertex.x, vertex.y);
    }
    
    println!("\n=== Workflow Complete! ===");
    println!("The floor plan is now fully constrained and ready for:");
    println!("  • Adding/trimming areas");
    println!("  • Wall shifting");
    println!("  • Room detection");
    println!("  • Export to CAD formats");
    
    Ok(())
}
