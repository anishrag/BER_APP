// Example: Smart Measurement Suggestions

use floorplan_geometry::*;

fn main() -> Result<(), Error> {
    println!("=== Smart Measurement System ===\n");
    println!("Instead of asking for 'p[0], p[1], p[2]...', the system suggests");
    println!("measurements that are actually easy to take in the real world.\n");
    
    // ========== Create an L-shaped floor plan ==========
    println!("Creating an L-shaped floor plan...");
    
    let l_shape = vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 5.0),
        Point::new(5.0, 5.0),
        Point::new(5.0, 10.0),
        Point::new(0.0, 10.0),
    ];
    
    let stroke = Stroke::new(l_shape);
    let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;
    
    println!("✓ Created plan with {} vertices", plan.polygon().vertex_count());
    println!("✓ Active parameters: {}\n", plan.get_active_parameters().len());
    
    // ========== Get smart measurement suggestions ==========
    println!("Getting smart measurement suggestions...\n");
    
    let suggestions = plan.suggest_measurements();
    
    println!("The system suggests {} measurements:\n", suggestions.len());
    println!("{:-<70}", "");
    
    for (i, suggestion) in suggestions.iter().enumerate() {
        println!("\n{}. {} (Priority {})", 
            i + 1, 
            suggestion.description,
            suggestion.priority
        );
        
        println!("   Type: {:?}", suggestion.measurement_type);
        
        if let (Some(from), Some(to)) = (&suggestion.from_point, &suggestion.to_point) {
            println!("   Measure from ({:.1}, {:.1}) to ({:.1}, {:.1})",
                from.x, from.y, to.x, to.y);
            let distance = from.distance_to(to);
            println!("   Approximate distance: {:.1} units", distance);
        }
        
        println!("   Sets parameter(s): {:?}", suggestion.target_parameters);
        
        if !suggestion.alternatives.is_empty() {
            println!("   Alternatives:");
            for alt in &suggestion.alternatives {
                println!("     - {} ({})", alt.description, alt.computation);
            }
        }
    }
    
    println!("\n{:-<70}", "");
    
    // ========== Compare with naive approach ==========
    println!("\nComparison:\n");
    
    println!("❌ NAIVE APPROACH:");
    println!("   'Please provide values for parameters:'");
    for (i, param_id) in plan.get_active_parameters().iter().enumerate() {
        println!("   {}. p[{}] = ?", i + 1, param_id);
    }
    println!("   (User has no idea what these mean!)\n");
    
    println!("✅ SMART APPROACH:");
    println!("   'Please take these measurements:'");
    for (i, instruction) in plan.get_measurement_instructions().iter().take(3).enumerate() {
        println!("   {}. {}", i + 1, instruction);
    }
    println!("   (User knows exactly what to measure!)\n");
    
    // ========== Demonstrate measurement types ==========
    println!("{:=<70}", "");
    println!("MEASUREMENT TYPE EXAMPLES");
    println!("{:=<70}\n", "");
    
    // Group by type
    use std::collections::HashMap;
    let mut by_type: HashMap<String, Vec<&measurements::MeasurementSuggestion>> = HashMap::new();
    
    for suggestion in &suggestions {
        let type_name = format!("{:?}", suggestion.measurement_type);
        by_type.entry(type_name).or_insert_with(Vec::new).push(suggestion);
    }
    
    for (type_name, suggs) in by_type.iter() {
        println!("📏 {}:", type_name);
        for sugg in suggs {
            println!("   - {}", sugg.description);
        }
        println!();
    }
    
    // ========== Simulate user taking measurements ==========
    println!("{:=<70}", "");
    println!("SIMULATING USER MEASUREMENTS");
    println!("{:=<70}\n", "");
    
    println!("User takes measurements in priority order:\n");
    
    for (i, suggestion) in suggestions.iter().take(3).enumerate() {
        // Simulate user measuring
        let measured_value = match suggestion.measurement_type {
            measurements::MeasurementType::EdgeLength => 8.0 + i as f64,
            measurements::MeasurementType::RoomDimension => 12.0,
            measurements::MeasurementType::IncrementalOffset => 3.5,
            measurements::MeasurementType::OverallDimension => 15.0,
            measurements::MeasurementType::AbsoluteFromOrigin => 5.0,
        };
        
        println!("{}. User measures: '{}'", i + 1, suggestion.description);
        println!("   Result: {:.1} units", measured_value);
        
        plan.apply_measurement(suggestion, measured_value)?;
        
        println!("   ✓ Applied to parameter(s) {:?}\n", suggestion.target_parameters);
    }
    
    // ========== Show the result ==========
    println!("After applying measurements:");
    println!("  Parameters set: {} / {}", 
        plan.parametric.parameters.len(),
        plan.get_active_parameters().len()
    );
    
    let polygon = plan.polygon();
    println!("  Plan area: {:.2} square units", polygon.area());
    println!("  Plan perimeter: {:.2} units", utils::perimeter(&polygon));
    
    // ========== Advanced example: Incremental measurement ==========
    println!("\n{:=<70}", "");
    println!("ADVANCED: INCREMENTAL MEASUREMENTS");
    println!("{:=<70}\n", "");
    
    println!("Instead of asking for absolute coordinates, the system can suggest");
    println!("incremental measurements from already-known points:");
    println!();
    println!("Example:");
    println!("  ❌ Bad:  'Measure distance from origin to corner C'");
    println!("  ✅ Good: 'Measure distance from corner B to corner C'");
    println!("           (Because you already measured to B!)");
    println!();
    println!("This is especially helpful when:");
    println!("  • Origin is not easily accessible");
    println!("  • You're measuring room-by-room");
    println!("  • Working in existing buildings");
    
    // ========== Summary ==========
    println!("\n{:=<70}", "");
    println!("KEY BENEFITS");
    println!("{:=<70}\n", "");
    
    println!("✓ User-friendly: Asks for measurements that make sense");
    println!("✓ Prioritized: Easiest measurements first");
    println!("✓ Flexible: Provides alternatives when available");
    println!("✓ Context-aware: Understands room dimensions vs edge lengths");
    println!("✓ Incremental: Builds on previous measurements");
    println!("✓ Visual hints: Shows exactly where to measure");
    
    println!("\n{:=<70}", "");
    
    Ok(())
}
