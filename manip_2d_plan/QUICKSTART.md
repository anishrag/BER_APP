# Floorplan Geometry - Quick Reference

## Common Usage Patterns

### Creating a Floor Plan

```rust
// From orthogonal stroke (all edges parallel/perpendicular)
let plan = FloorPlan::from_stroke_orthogonal(&stroke, tolerance)?;

// From freeform stroke
let plan = FloorPlan::from_stroke_freeform(&stroke, tolerance)?;
```

### Adding Areas

```rust
// With orthogonal constraint
let new_plan = plan.add_area_orthogonal(&stroke)?;

// Without constraint
let new_plan = plan.add_area_freeform(&stroke)?;
```

### Trimming Areas

```rust
// Trim by stroke
let new_plan = plan.trim_area_orthogonal(&stroke)?;

// Trim by line (keeps largest part)
let new_plan = plan.trim_by_line(&line)?;
```

### Working with Constraints

```rust
// Get suggestions
let suggestions = plan.suggest_collinear_constraints(tolerance);

// Add constraints
plan.add_collinear_constraint(edge1, edge2)?;
plan.constraints.add_parallel(edge1, edge2)?;
plan.constraints.add_perpendicular(edge1, edge2)?;
plan.constraints.add_fixed_length(edge, length)?;

// Cassowary solver automatically applies all constraints simultaneously
// with priority-based conflict resolution (REQUIRED > STRONG > WEAK)
plan.apply_constraints()?;
```

**Note:** The crate uses the Cassowary constraint solver (same algorithm as Apple's Auto Layout) for robust, deterministic constraint solving. See `CASSOWARY.md` for details.

### Dimensions

```rust
// Get required dimensions
let required = plan.required_dimensions();

// Apply dimension
let dimension = Dimension {
    kind: DimensionKind::EdgeLength { edge_index: 0 },
    value: 12.0,
};
plan.set_dimension(dimension)?;
```

### Wall Operations

```rust
// Shift a wall
plan.shift_wall(edge_index, offset)?;
```

### Room Detection

```rust
// Detect rooms
plan.detect_rooms()?;

// Access rooms
for room in &plan.rooms {
    println!("Area: {}", room.area);
    let suggestions = rooms::suggest_room_type(room);
}
```

## Key Types

- `Point` - 2D coordinate
- `Line` - Line segment between two points
- `Stroke` - Raw user input (list of points)
- `Polygon` - Closed shape (list of vertices)
- `FloorPlan` - Complete floor plan with constraints and rooms
- `Room` - Detected room within a floor plan

## Important Functions

### Stroke Operations
- `Stroke::new(points)` - Create stroke
- `stroke.is_closed(tolerance)` - Check if closed
- `stroke.resample(spacing)` - Resample with even spacing
- `stroke.total_length()` - Calculate total length

### Simplification
- `simplification::simplify_stroke(&stroke, tolerance)` - Simplify using RDP
- `simplification::simplify_to_polygon(&stroke, tolerance)` - Convert to polygon

### Orthogonal
- `orthogonal::make_orthogonal(&stroke, tolerance)` - Force parallel/perpendicular

### Snapping
- `snapping::snap_to_polygon(&point, &polygon, max_dist)` - Snap point to polygon
- `snapping::find_intersection_points(&points, &polygon)` - Find intersections

### Utilities
- `utils::are_parallel(&line1, &line2, tolerance)` - Check parallelism
- `utils::are_perpendicular(&line1, &line2, tolerance)` - Check perpendicularity
- `utils::perimeter(&polygon)` - Calculate perimeter
- `utils::is_ccw(&polygon)` - Check counter-clockwise winding

## Tolerances

Typical tolerance values:
- **Simplification**: 0.5 - 2.0 (units in your coordinate system)
- **Snapping**: 3.0 - 10.0 (distance threshold)
- **Angle tolerance**: 0.1 - 0.2 radians (~6-12 degrees)
- **Collinear detection**: 0.5 - 1.0

## Error Handling

All fallible operations return `Result<T, Error>`. Common errors:
- `InsufficientPoints` - Stroke has too few points
- `InvalidPolygon` - Polygon validation failed
- `OrthogonalError` - Orthogonal conversion failed
- `SnappingError` - Snapping operation failed
- `ConstraintConflict` - Conflicting constraints
- `BooleanOpError` - Boolean operation failed

## Performance Tips

1. **Simplify early**: Reduce points before heavy operations
2. **Batch constraints**: Add multiple constraints before solving
3. **Reuse spatial indices**: Keep R-tree indices when querying multiple times
4. **Avoid repeated conversions**: Cache geo_types::Polygon if doing multiple boolean ops
5. **Set appropriate tolerances**: Too small = slow, too large = inaccurate

## Testing Your Code

```rust
#[test]
fn test_my_floor_plan() {
    let stroke = Stroke::new(vec![/* points */]);
    let plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5).unwrap();
    
    assert!(plan.polygon.vertex_count() >= 3);
    assert!(plan.polygon.area() > 0.0);
}
```

## Integration with UI

The crate is UI-agnostic. Typical integration:

1. **User draws**: Collect touch/mouse points → `Stroke`
2. **Simplify**: Convert to `FloorPlan`
3. **Render**: Iterate `plan.polygon.vertices` to draw
4. **Edit**: Use add/trim/shift operations
5. **Query**: Ask for dimension suggestions
6. **Update**: Apply user input via constraints/dimensions

## Next Steps

- Read the full README.md
- Run examples: `cargo run --example basic_usage`
- Check tests: `cargo test`
- Review source code in `src/` directory
