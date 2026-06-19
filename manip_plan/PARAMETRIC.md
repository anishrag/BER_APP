# Parametric Constraint System

## Overview

The parametric system represents floor plan geometry using a **minimal set of independent parameters** stored in a HashMap. All vertex coordinates are expressed as functions of these parameters.

## Key Concept

Instead of storing vertex positions directly and adding constraints, we store **parameters** and express vertices as **functions of parameters**.

### Traditional Approach (What We Had Before)
```rust
// Store vertices directly
vertices = [(0,0), (10,5), (10,10), (0,10)]

// Add constraints to limit their movement
constraints = [
    "edges 0 and 2 are horizontal",
    "edges 1 and 3 are vertical",
    "edge 0 has length 10",
]

// Problem: 8 values (4 vertices * 2 coords) + complex constraint solver
```

### Parametric Approach (What We Have Now)
```rust
// Store only independent parameters
parameters = HashMap {
    0 => 10.0,  // width
    1 => 10.0,  // height
}

// Express vertices as functions
vertices = [
    Vertex { x: Constant(0), y: Constant(0) },       // origin
    Vertex { x: Parameter(0), y: Constant(0) },      // (width, 0)
    Vertex { x: Parameter(0), y: Parameter(1) },     // (width, height)
    Vertex { x: Constant(0), y: Parameter(1) },      // (0, height)
]

// Benefits: Only 2 values! Constraints already encoded in expressions.
```

## How It Works

### 1. Degrees of Freedom (DOF)

For a polygon with `n` vertices:
- **Maximum DOF** = `n * 2 - 2` (minus 2 because we fix first vertex at origin)
- **After orthogonal constraints** = number of unique x and y coordinates
- **After collinear constraints** = further reduced as edges share coordinates

### 2. Parameter Reduction Through Constraints

**Example: Rectangle (4 vertices)**

Initial (no constraints):
```
DOF = 4*2 - 2 = 6 parameters
p[0] = x1, p[1] = y1
p[2] = x2, p[3] = y2  
p[4] = x3, p[5] = y3
```

After orthogonal constraints:
```
DOF = 2 parameters  
p[0] = width
p[1] = height

vertices = [
    (0, 0),              // fixed
    (p[0], 0),           // x = width, y = 0
    (p[0], p[1]),        // x = width, y = height
    (0, p[1]),           // x = 0, y = height
]
```

### 3. Coordinate Expressions

Each coordinate is one of:

```rust
enum CoordinateExpression {
    Constant(f64),                    // e.g., x = 0.0 (fixed)
    Parameter(id),                    // e.g., x = p[3]
    Sum(id1, id2),                    // e.g., x = p[1] + p[4]
    Difference(id1, id2),             // e.g., x = p[2] - p[5]
    Scaled(scale, id),                // e.g., x = 2.0 * p[1]
}
```

## Workflow

### Step 1: Create Orthogonal Plan

```rust
let plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;

// Automatically:
// 1. Creates parameters for unique x and y coordinates
// 2. Maps vertices to these parameters
// 3. Returns reduced parameter count
```

**What happens internally:**
- Identifies unique x-coordinates → creates parameters for each
- Identifies unique y-coordinates → creates parameters for each  
- Maps each vertex to (x_param, y_param)

### Step 2: Apply Collinear Constraints

```rust
plan.add_collinear_constraint(edge1, edge2)?;

// Makes edges share coordinates on one axis
// Reduces parameter count by reusing existing parameters
```

**Example:**
```
Before:
  Edge 0 start: (0, p[2])
  Edge 3 start: (0, p[5])

After collinear on y-axis:
  Edge 0 start: (0, p[2])
  Edge 3 start: (0, p[2])  // Now shares p[2]
  
Parameter p[5] is no longer needed!
```

### Step 3: Set Parameter Values

```rust
// Set directly
plan.set_parameter(0, 15.0);

// Or by edge
plan.set_edge_length(0, 15.0)?;
```

**Moving a wall is just changing a HashMap value!**

## Advanced Features

### Adding Non-Orthogonal Vertices (Chamfers, Cuts)

```rust
// Add a chamfer corner
let (x_param, y_param) = plan.parametric.add_vertex_at(
    index, 
    Point::new(9.0, 1.0)
)?;

// This:
// 1. Creates 2 new parameters (x and y are now independent)
// 2. Inserts vertex with expressions Parameter(x_param), Parameter(y_param)
// 3. Increases DOF by 2
```

### Removing Vertices

```rust
plan.parametric.remove_vertex(index)?;

// Note: Parameters aren't automatically reclaimed
// (they might be referenced by other vertices)
```

## Benefits

### 1. Efficient Wall Movement
```rust
// OLD: Rerun entire constraint solver
plan.shift_wall(edge, offset)?;  // Expensive!

// NEW: Change one HashMap value
plan.set_parameter(param_id, new_value);  // O(1)
```

### 2. Clear DOF Tracking
```rust
// Know exactly how many dimensions user needs to provide
let unset = plan.calculate_required_parameters();
println!("User needs to provide {} dimensions", unset.len());
```

### 3. Easy Serialization
```rust
// Save/load is just the HashMap + expressions
let saved = serde_json::to_string(&plan.parametric)?;
```

### 4. Predictable Behavior
- No constraint solver convergence issues
- No constraint conflicts
- Direct mapping: parameter → geometry

## Implementation Details

### Parameter Sharing (Orthogonal)

When applying orthogonal constraints, vertices that should have the same x or y coordinate share a parameter:

```rust
// Group by approximate x-value
unique_x = [(0.0, p[0]), (10.0, p[1]), (10.0, p[1])]
                                    // ^ These share p[1]

// Vertices at x=10 all use p[1]
```

### Collinear Edge Handling

Collinear edges on a horizontal line share y-coordinates:

```rust
// Edge 0: (p[0], p[2]) -> (p[1], p[2])
// Edge 5: (p[3], p[7]) -> (p[4], p[7])

// After marking collinear:
// Edge 5: (p[3], p[2]) -> (p[4], p[2])
//               ^^^^^ Now uses p[2] instead of p[7]
```

### Edge Length Parameters

For orthogonal edges, length is controlled by one parameter:

```rust
// Horizontal edge from (p[0], p[2]) to (p[1], p[2])
// Length = p[1] - p[0]
// If p[0]=0 (constant), then length = p[1]

plan.set_parameter(1, 15.0);  // Edge is now 15 units long
```

## Comparison with Cassowary

| Aspect | Cassowary | Parametric |
|--------|-----------|------------|
| **Approach** | Constraint solver | Direct parametrization |
| **DOF representation** | Implicit (through constraints) | Explicit (HashMap) |
| **Wall movement** | Re-solve constraints | Change one value |
| **Add/remove features** | Add constraints | Add/remove parameters |
| **Complexity** | O(n²) solver | O(1) evaluation |
| **Best for** | Complex, over-constrained systems | Clean, minimal DOF systems |

## When to Use Each

**Use Parametric (what we have now):**
- ✅ Orthogonal floor plans
- ✅ Clear DOF requirements
- ✅ Interactive wall movement
- ✅ Adding/removing features dynamically

**Use Cassowary (backup if needed):**
- ⚠️ Over-constrained systems
- ⚠️ Conflicting user requirements
- ⚠️ Non-orthogonal complex constraints

For floor plans, **parametric is the right choice** because:
1. DOF are naturally minimal (orthogonal + collinear)
2. User interaction is about changing parameter values
3. No need for complex constraint solving

## Code Example

Complete workflow:

```rust
// 1. Create plan
let mut plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;
println!("Parameters: {}", plan.get_active_parameters().len());

// 2. Apply collinear
let suggestions = plan.suggest_collinear_constraints(0.5);
for suggestion in suggestions {
    plan.add_collinear_constraint(suggestion.edge1, suggestion.edge2)?;
}
println!("Parameters after collinear: {}", plan.get_active_parameters().len());

// 3. Set dimensions
let unset = plan.calculate_required_parameters();
for param_id in unset {
    let value = get_user_input(format!("Enter value for p[{}]", param_id));
    plan.set_parameter(param_id, value);
}

// 4. Use the plan
let polygon = plan.polygon();  // Computed from parameters
println!("Area: {}", polygon.area());

// 5. Move a wall
plan.set_parameter(0, 20.0);  // Change width to 20
let new_polygon = plan.polygon();
println!("New area: {}", new_polygon.area());
```

## Summary

The parametric system is **elegant** because:
- Vertices are computed, not stored
- Parameters = actual degrees of freedom
- Constraints are encoded in expressions, not solved
- Wall movement is trivial
- Serialization is simple

This matches perfectly with how floor plans work in practice!
