# Floorplan Geometry

A Rust library for processing hand-drawn floor plan strokes into precise geometric plans with intelligent simplification, constraint management, and room detection.

## Features

- **Stroke Simplification**: Convert rough hand-drawn strokes into clean polygons using the Ramer-Douglas-Peucker algorithm
- **Orthogonal Constraints**: Automatically align edges to be parallel or perpendicular (Manhattan-world assumption)
- **Smart Snapping**: Intelligently snap new geometry to existing plans
- **Geometric Constraints**: Manage parallel, perpendicular, collinear, and fixed-length constraints
- **Add/Trim Operations**: Boolean operations for adding and removing areas from plans
- **Dimension Management**: Calculate required dimensions and apply user-provided measurements
- **Wall Shifting**: Move walls while maintaining connections and constraints
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

```rust
use floorplan_geometry::*;

// Create a floor plan from a hand-drawn stroke
let points = vec![
    Point::new(0.0, 0.0),
    Point::new(10.2, 0.3),
    Point::new(10.1, 10.1),
    Point::new(0.1, 9.9),
];
let stroke = Stroke::new(points);

// Simplify to an orthogonal polygon (all edges parallel/perpendicular)
let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;

// Or create without orthogonal constraint
let plan_freeform = FloorPlan::from_stroke_freeform(&stroke, 0.5)?;

// Add an area
let addition_points = vec![
    Point::new(10.0, 5.0),
    Point::new(15.0, 5.0),
    Point::new(15.0, 10.0),
    Point::new(10.0, 10.0),
];
let addition_stroke = Stroke::new(addition_points);
let updated_plan = plan.add_area_orthogonal(&addition_stroke)?;

// Set dimensions
let dimension = Dimension {
    kind: DimensionKind::EdgeLength { edge_index: 0 },
    value: 12.5, // meters
};
plan.set_dimension(dimension)?;

// Detect rooms
plan.detect_rooms()?;
```

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
- `nalgebra` - Linear algebra for constraint solving
- `thiserror` - Error handling

## Examples

See the `examples/` directory for complete examples:

- `basic_usage.rs` - Simple floor plan creation
- `add_trim.rs` - Adding and removing areas
- `constraints.rs` - Working with geometric constraints
- `dimensions.rs` - Dimension management
- `rooms.rs` - Room detection and labeling

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
