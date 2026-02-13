# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- **MAJOR**: Complete rewrite using **parametric representation**
  - Floor plans now stored as HashMap of parameters (DOF)
  - Vertex coordinates computed from parameter expressions
  - Moving walls = changing one HashMap value (O(1) instead of O(n²))
  - No constraint solver needed - geometry is directly parametrized
- **BREAKING**: FloorPlan now uses `parametric::ParametricPlan` internally
  - Access geometry via `plan.polygon()` instead of `plan.polygon`
  - Set dimensions via `plan.set_parameter(id, value)` or `plan.set_edge_length(edge, length)`
  - Parameters tracked explicitly: `plan.get_active_parameters()`
- Simplified constraint types (Horizontal, Vertical, Collinear only)
- Orthogonal constraints now reduce parameters instead of adding constraints

### Added
- **New module**: `parametric` - Parametric representation system
- `ParametricPlan` - Core parametric floor plan type
- `CoordinateExpression` - Defines coordinates as functions of parameters
- `FloorPlan::set_parameter()` - Direct parameter control
- `FloorPlan::get_edge_parameter()` - Find which parameter controls an edge
- `FloorPlan::set_edge_length()` - Convenience method for setting edge lengths
- `ParametricPlan::add_vertex_at()` - Add non-orthogonal features (chamfers, cuts)
- `ParametricPlan::remove_vertex()` - Remove vertices dynamically
- New example: `parametric_demo.rs` - Comprehensive parametric system demo
- PARAMETRIC.md - Full documentation of the parametric system

### Improved
- **Performance**: Wall movement is now O(1) instead of O(n²)
- **Clarity**: DOF are explicit (HashMap size) instead of implicit (constraint count)
- **Flexibility**: Easy to add/remove vertices and features dynamically
- **Serialization**: Just save the HashMap + expressions

### Removed
- Cassowary dependency (no longer needed with parametric representation)
- Complex constraint solver
- Parallel/Perpendicular/EqualLength constraint types

## [0.1.0] - 2024-02-12

### Added
- Initial release
- Stroke simplification using Ramer-Douglas-Peucker algorithm
- Orthogonal polygon creation (parallel/perpendicular constraint enforcement)
- Freeform polygon simplification
- Smart snapping to existing geometry
- Geometric constraint system (parallel, perpendicular, collinear, fixed length)
- Add area operations with boolean union
- Trim area operations with boolean difference
- Trim by line operation
- Wall shifting while maintaining connections
- Dimension management and calculation
- Room detection and labeling
- Comprehensive utility functions
- Full test suite
- Documentation and examples

### Features
- `Stroke`: Hand-drawn input representation
- `FloorPlan`: Main container for floor plan geometry
- `Polygon`: Closed polygon representation
- `ConstraintSet`: Geometric constraint management
- `Room`: Room detection and properties
- Snapping system for connecting geometry
- Dimension calculation and application
- Boolean operations (union, difference)

### Dependencies
- geo (geometry algorithms)
- geo-types (shared types)
- rstar (spatial indexing)
- rdp (stroke simplification)
- nalgebra (linear algebra)
- thiserror (error handling)

[Unreleased]: https://github.com/yourusername/floorplan_geometry/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/yourusername/floorplan_geometry/releases/tag/v0.1.0
