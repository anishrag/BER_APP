# Floorplan Geometry

A Rust library for processing hand-drawn floor plan strokes into precise geometric plans with intelligent simplification, constraint management, and room detection.

## Features

- **Parametric Representation**: Floor plans stored as minimal independent parameters in a HashMap
- **Stroke Simplification**: Convert rough hand-drawn strokes into clean polygons using the Ramer-Douglas-Peucker algorithm
- **Orthogonal Constraints**: Automatically align edges and reduce parameters to unique x/y coordinates
- **Smart Snapping**: Intelligently snap new geometry to existing plans
- **Collinear Constraints**: Share parameters between edges on the same line
- **Direct Parameter Control**: Move walls by changing a single HashMap value (no constraint re-solving)
- **Add/Trim Operations**: Boolean operations for adding and removing areas from plans
- **Dynamic Features**: Add non-orthogonal vertices (chamfers, cuts) with automatic parameter management
- **Room Detection**: Identify and label rooms within floor plans

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
floorplan_geometry = "0.1.0"

# Or with serialization support
floorplan_geometry = { version = "0.1.0", features = ["serde"] }
```

## Quick Start

### Recommended Workflow

```rust
use floorplan_geometry::*;

// STEP 1: Create orthogonal plan from user sketch
// This automatically adds horizontal/vertical constraints for all edges
let points = vec![
    Point::new(0.0, 0.0),
    Point::new(10.2, 0.3),
    Point::new(10.1, 10.1),
    Point::new(0.1, 9.9),
];
let stroke = Stroke::new(points);
let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;

// STEP 2: Prompt user to confirm collinear edges
let suggestions = plan.suggest_collinear_constraints(0.5);
for suggestion in suggestions {
    if user_confirms(&suggestion) {  // Your UI logic
        plan.add_collinear_constraint(suggestion.edge1, suggestion.edge2)?;
    }
}

// STEP 3: Calculate minimum dimensions needed for full constraint
let required = plan.calculate_required_dimensions();
println!("Need {} critical dimensions", 
    required.iter().filter(|d| d.priority == DimensionPriority::Critical).count()
);

// STEP 4: Get dimension values from user and apply them
for dim_req in required.iter().filter(|d| d.priority == DimensionPriority::Critical) {
    let value = get_user_input(&dim_req.description);  // Your UI logic
    plan.set_dimension(Dimension {
        kind: dim_req.kind.clone(),
        value,
    })?;
}

// Plan is now fully constrained and ready for use!
```

See `examples/workflow.rs` for a complete demonstration.

## Core Concepts

### Strokes

A `Stroke` represents raw user input - a sequence of points drawn on screen:

```rust
let stroke = Stroke::new(vec![
    Point::new(0.0, 0.0),
    Point::new(5.0, 0.1),
    Point::new(10.0, 0.0),
]);

// Check properties
if stroke.is_closed(0.5) {
    println!("Stroke forms a closed loop");
}

// Resample for even spacing
let resampled = stroke.resample(1.0)?;
```

### Floor Plans

A `FloorPlan` is the main structure containing the simplified polygon, constraints, and rooms:

```rust
let plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;

// Access the polygon
println!("Vertices: {:?}", plan.polygon.vertices);

// Get dimension requirements
let required = plan.required_dimensions();
for dim in required {
    println!("{}: current value = {}", dim.description, dim.current_value);
}
```

### Constraints

The library supports various geometric constraints:

```rust
// Suggest collinear edges
let suggestions = plan.suggest_collinear_constraints(0.5);
for suggestion in suggestions {
    if suggestion.confidence > 0.8 {
        plan.add_collinear_constraint(suggestion.edge1, suggestion.edge2)?;
    }
}

// Add custom constraints
plan.constraints.add_parallel(0, 2)?;
plan.constraints.add_fixed_length(0, 10.0)?;
```

## Advanced Usage

### Adding and Trimming Areas

```rust
// Add an area with automatic snapping
let new_area = Stroke::new(vec![...]);
let plan = plan.add_area_orthogonal(&new_area)?;

// Trim by a freeform stroke
let trim_stroke = Stroke::new(vec![...]);
let plan = plan.trim_area_freeform(&trim_stroke)?;

// Trim by a line, keeping the largest part
let cut_line = Line::new(Point::new(0.0, 5.0), Point::new(10.0, 5.0));
let plan = plan.trim_by_line(&cut_line)?;
```

### Wall Shifting

```rust
// Shift a wall while maintaining connections
plan.shift_wall(
    edge_index: 0,
    offset: 2.0, // Positive = perpendicular direction
)?;
```

### Room Detection

```rust
// Detect rooms within the plan
plan.detect_rooms()?;

// Access room information
for room in &plan.rooms {
    println!("Room area: {}", room.area);
    println!("Room center: {:?}", room.center);
    
    // Get suggestions for room type
    let suggestions = rooms::suggest_room_type(room);
    println!("Possible room types: {:?}", suggestions);
}
```

### Dimension Management

```rust
// Get required dimensions for full constraint
let required = plan.required_dimensions();

// Apply user-provided dimensions
for req in required {
    if req.priority == DimensionPriority::Critical {
        let user_value = get_user_input(&req.description);
        plan.set_dimension(Dimension {
            kind: req.kind,
            value: user_value,
        })?;
    }
}
```

## Algorithm Details

### Simplification

The library uses the Ramer-Douglas-Peucker (RDP) algorithm for stroke simplification, which:
- Preserves critical corner points
- Removes redundant intermediate points
- Maintains the overall shape with configurable tolerance

### Orthogonalization

The orthogonal constraint enforcement:
1. Identifies the dominant angle of the stroke
2. Rotates to align with cardinal directions
3. Snaps all movements to horizontal/vertical
4. Rotates back to the original orientation

This creates clean rectangular structures from rough sketches.

### Snapping

The snapping system:
- Prioritizes vertex-to-vertex snapping
- Falls back to edge snapping for continuity
- Uses configurable distance thresholds
- Maintains geometric integrity during operations

### Constraint Solving

The constraint solver uses the **Cassowary algorithm**, which:
- Solves multiple constraints simultaneously
- Handles constraint priorities (REQUIRED > STRONG > WEAK)
- Resolves conflicts automatically
- Efficiently finds optimal solutions for linear constraints

The solver applies both native linear constraints and linearized approximations of geometric constraints. For most floor plan applications, this produces excellent results in a single solve.

Example with conflicting constraints:
```rust
// Fix vertex at origin (REQUIRED priority)
plan.constraints.constraints.push(
    Constraint::FixedVertex { vertex: 0, position: Point::new(0.0, 0.0) }
);

// Set edge length (STRONG priority)
plan.constraints.add_fixed_length(0, 10.0)?;

// Also try to fix end vertex at (8, 0) - conflicts with length!
plan.constraints.constraints.push(
    Constraint::FixedVertex { vertex: 1, position: Point::new(8.0, 0.0) }
);

// Cassowary resolves: REQUIRED vertices win, edge length adjusts
plan.apply_constraints()?;
```

See `examples/cassowary_demo.rs` for a comprehensive demonstration.

### Constraint Solving

The constraint solver:
- Applies constraints iteratively
- Converges to a stable solution
- Maintains topological relationships
- Handles over-constrained systems gracefully

## Dependencies

- `geo` - Core geometric types and algorithms
- `geo-types` - Shared geometry types
- `rstar` - Spatial indexing for efficient queries
- `rdp` - Ramer-Douglas-Peucker simplification
- `cassowary` - Linear constraint solver (Cassowary algorithm)
- `nalgebra` - Linear algebra for helpers
- `thiserror` - Error handling

## Examples

See the `examples/` directory for complete examples:

- **`workflow.rs`** - **Recommended workflow** showing the 4-step process
- `basic_usage.rs` - Simple floor plan creation and manipulation
- `constraints.rs` - Working with geometric constraints  
- `cassowary_demo.rs` - Cassowary constraint solver demonstration

Run an example:
```bash
cargo run --example workflow
```

## Testing

Run the test suite:

```bash
cargo test
```

Run with coverage:

```bash
cargo tarpaulin --out Html
```

## Performance Considerations

- Stroke simplification is O(n log n) with RDP
- Boolean operations use efficient sweep-line algorithms
- Spatial queries leverage R-tree indexing
- Constraint solving converges in typically < 100 iterations

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Future Enhancements

Planned features include:

- [ ] Advanced room segmentation with internal walls
- [ ] Door and window detection
- [ ] Furniture placement assistance
- [ ] 3D extrusion capabilities
- [ ] SVG/DXF export
- [ ] Machine learning-based stroke recognition
- [ ] Real-time collaborative editing
- [ ] Undo/redo support with history
- [ ] Advanced constraint solver with optimization

## Authors

Your Name - Initial work

## Acknowledgments

- Thanks to the `geo` crate authors for geometric primitives
- RDP algorithm implementation based on the original 1973 paper
- Inspired by professional CAD and architecture software
