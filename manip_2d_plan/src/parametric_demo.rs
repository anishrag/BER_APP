// Example: Parametric constraint system

use floorplan_geometry::*;

fn main() -> Result<(), Error> {
    println!("=== Parametric Constraint System ===\n");
    println!("This system uses a HashMap of parameters where:");
    println!("- Each coordinate is expressed as a function of parameters");
    println!("- Constraints reduce the number of independent parameters");
    println!("- Moving a wall = changing a single parameter value\n");
    
    // ========== STEP 1: Create Plan ==========
    println!("STEP 1: Create orthogonal plan");
    println!("-------------------------------------------");
    
    let sketch = vec![
        Point::new(0.0, 0.0),
        Point::new(10.2, 0.1),
        Point::new(10.0, 5.1),
        Point::new(0.1, 4.9),
    ];
    
    let stroke = Stroke::new(sketch);
    let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;
    
    println!("Created plan with {} vertices", plan.polygon().vertex_count());
    println!("Total theoretical DOF: {} (vertices * 2 - 2)", 
        plan.polygon().vertex_count() * 2 - 2);
    
    // ========== STEP 2: Check Parameters After Orthogonal ==========
    println!("\nSTEP 2: Parameters after orthogonal constraints");
    println!("-------------------------------------------");
    
    let active_params = plan.get_active_parameters();
    println!("Active parameters: {}", active_params.len());
    println!("Parameter values:");
    
    for param_id in &active_params {
        if let Some(value) = plan.get_parameter(*param_id) {
            println!("  p[{}] = {:.2}", param_id, value);
        }
    }
    
    // Show how vertices map to parameters
    println!("\nVertex expressions:");
    for (i, vertex) in plan.parametric.vertices.iter().enumerate() {
        println!("  Vertex {}: x={:?}, y={:?}", i, vertex.x_expr, vertex.y_expr);
    }
    
    // ========== STEP 3: Identify Edge Parameters ==========
    println!("\nSTEP 3: Edge length parameters");
    println!("-------------------------------------------");
    
    let polygon = plan.polygon();
    for i in 0..polygon.vertex_count() {
        if let Some(param_id) = plan.get_edge_parameter(i) {
            let edge = polygon.edge(i).unwrap();
            let current_length = edge.length();
            println!("  Edge {}: controlled by p[{}] = {:.2}", i, param_id, current_length);
        } else {
            println!("  Edge {}: derived from multiple parameters", i);
        }
    }
    
    // ========== STEP 4: Change a Parameter (Move a Wall) ==========
    println!("\nSTEP 4: Move a wall by changing parameter");
    println!("-------------------------------------------");
    
    println!("Original area: {:.2}", plan.polygon().area());
    
    // Change the first parameter (width)
    if let Some(&first_param) = active_params.first() {
        println!("Changing p[{}] from {:.2} to 15.0", 
            first_param, 
            plan.get_parameter(first_param).unwrap()
        );
        
        plan.set_parameter(first_param, 15.0);
        
        println!("New area: {:.2}", plan.polygon().area());
        println!("\nUpdated vertices:");
        for (i, vertex) in plan.polygon().vertices.iter().enumerate() {
            println!("  Vertex {}: ({:.2}, {:.2})", i, vertex.x, vertex.y);
        }
    }
    
    // ========== STEP 5: Add Collinear Constraint ==========
    println!("\nSTEP 5: Apply collinear constraint");
    println!("-------------------------------------------");
    
    // Create a more complex shape for collinearity demo
    let l_shape = vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 5.0),
        Point::new(5.0, 5.0),
        Point::new(5.0, 10.0),
        Point::new(0.0, 10.0),
    ];
    
    let mut l_plan = FloorPlan::from_stroke_orthogonal(
        &Stroke::new(l_shape), 0.5
    )?;
    
    println!("L-shaped plan created");
    println!("Parameters before collinear: {}", l_plan.get_active_parameters().len());
    
    // Check for collinear suggestions
    let suggestions = l_plan.suggest_collinear_constraints(0.5);
    if let Some(suggestion) = suggestions.first() {
        println!("Applying collinear constraint: edges {} and {}", 
            suggestion.edge1, suggestion.edge2);
        
        l_plan.add_collinear_constraint(suggestion.edge1, suggestion.edge2)?;
        
        println!("Parameters after collinear: {}", l_plan.get_active_parameters().len());
    }
    
    // ========== STEP 6: Set Edge Length Directly ==========
    println!("\nSTEP 6: Set edge length directly");
    println!("-------------------------------------------");
    
    let polygon = l_plan.polygon();
    for i in 0..polygon.vertex_count() {
        if let Some(_param) = l_plan.get_edge_parameter(i) {
            println!("Setting edge {} to length 12.0", i);
            l_plan.set_edge_length(i, 12.0)?;
            
            let new_edge = l_plan.polygon().edge(i).unwrap();
            println!("  New length: {:.2}", new_edge.length());
            break;
        }
    }
    
    // ========== STEP 7: Add Non-Orthogonal Feature ==========
    println!("\nSTEP 7: Add non-orthogonal vertex (chamfer)");
    println!("-------------------------------------------");
    
    println!("Parameters before adding vertex: {}", l_plan.get_active_parameters().len());
    
    // Add a chamfer by inserting a vertex
    let chamfer_pos = Point::new(9.0, 1.0);
    let (_x_param, _y_param) = l_plan.parametric.add_vertex_at(2, chamfer_pos)?;
    
    println!("Parameters after adding vertex: {}", l_plan.get_active_parameters().len());
    println!("Added 2 new DOF for the chamfered corner");
    
    println!("\nNew vertex positions:");
    for (i, vertex) in l_plan.polygon().vertices.iter().enumerate() {
        println!("  Vertex {}: ({:.2}, {:.2})", i, vertex.x, vertex.y);
    }
    
    // ========== Summary ==========
    println!("\n=== Key Benefits of Parametric System ===");
    println!("✓ Parameters = actual DOF (not redundant constraints)");
    println!("✓ Moving walls = changing one value in HashMap");
    println!("✓ Adding features = adding parameters, not rebuilding constraints");
    println!("✓ Vertex positions computed on-demand from parameters");
    println!("✓ Easy to serialize/deserialize (just save the HashMap)");
    
    Ok(())
}
