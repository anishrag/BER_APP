//! Smart measurement suggestion system
//! 
//! This module analyzes the floor plan and suggests measurements that are
//! easy for users to actually take in the real world, rather than always
//! asking for absolute coordinates from origin.
//! 
//! For example:
//! - Instead of "p[3] (distance from origin)", suggest "width of room 2"
//! - Instead of "p[5]", suggest "p[5] - p[2] (length of wall between corners)"
//! - Prefer edge lengths over absolute positions
//! - Prefer incremental measurements (from last known point)

use crate::{FloorPlan, Point, Line, Error};
use crate::parametric::{ParameterId, CoordinateExpression};
use std::collections::{HashMap, HashSet};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Type of measurement that's easy for users to take
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum MeasurementType {
    /// Direct edge length (easiest - just measure the wall)
    EdgeLength,
    
    /// Distance between two parallel edges (room width/depth)
    RoomDimension,
    
    /// Offset from a previous measurement point (incremental)
    IncrementalOffset,
    
    /// Overall building dimension (can measure from outside)
    OverallDimension,
    
    /// Absolute coordinate from origin (hardest - requires reference point)
    AbsoluteFromOrigin,
}

/// A suggested measurement for the user to take
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct MeasurementSuggestion {
    /// What parameter(s) this measurement will determine
    pub target_parameters: Vec<ParameterId>,
    
    /// Type of measurement
    pub measurement_type: MeasurementType,
    
    /// Human-readable description
    pub description: String,
    
    /// Visual hint for where to measure
    pub from_point: Option<Point>,
    pub to_point: Option<Point>,
    
    /// Priority (lower = ask first)
    pub priority: u32,
    
    /// Alternative measurements that could provide the same information
    pub alternatives: Vec<AlternativeMeasurement>,
}

/// An alternative way to measure the same thing
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct AlternativeMeasurement {
    pub description: String,
    pub measurement_type: MeasurementType,
    /// How to compute target parameter from this measurement
    /// e.g., "target = measured_value - p[2]"
    pub computation: String,
    pub depends_on: Vec<ParameterId>,
}

/// Calculate smart measurement suggestions for a floor plan
pub fn suggest_measurements(plan: &FloorPlan) -> Vec<MeasurementSuggestion> {
    let mut suggestions = Vec::new();
    
    let polygon = plan.polygon();
    let unset_params = plan.parametric.get_unset_parameters();
    let set_params: HashSet<_> = plan.parametric.parameters.keys().copied().collect();
    
    // Track which parameters we've suggested measurements for
    let mut covered: HashSet<ParameterId> = HashSet::new();
    
    // Priority 1: Edge lengths (easiest to measure)
    for edge_idx in 0..polygon.vertex_count() {
        if let Some(param_id) = plan.parametric.get_edge_length_parameter(edge_idx) {
            if unset_params.contains(&param_id) && !covered.contains(&param_id) {
                let edge = polygon.edge(edge_idx).unwrap();
                
                suggestions.push(MeasurementSuggestion {
                    target_parameters: vec![param_id],
                    measurement_type: MeasurementType::EdgeLength,
                    description: format!("Length of wall {} (edge {})", 
                        get_wall_name(&polygon, edge_idx), edge_idx),
                    from_point: Some(edge.start),
                    to_point: Some(edge.end),
                    priority: 1,
                    alternatives: vec![],
                });
                
                covered.insert(param_id);
            }
        }
    }
    
    // Priority 2: Room dimensions (width/depth between parallel walls)
    let room_dims = find_room_dimensions(plan);
    for (param_id, room_dim) in room_dims {
        if unset_params.contains(&param_id) && !covered.contains(&param_id) {
            suggestions.push(room_dim);
            covered.insert(param_id);
        }
    }
    
    // Priority 3: Incremental measurements (from already-measured points)
    for param_id in &unset_params {
        if covered.contains(param_id) {
            continue;
        }
        
        // Find if this parameter can be expressed as an offset from a known parameter
        if let Some(incremental) = find_incremental_measurement(
            plan, *param_id, &set_params
        ) {
            suggestions.push(incremental);
            covered.insert(*param_id);
        }
    }
    
    // Priority 4: Overall dimensions (can measure from outside)
    let overall = find_overall_dimensions(plan, &unset_params, &covered);
    for sugg in overall {
        for param in &sugg.target_parameters {
            covered.insert(*param);
        }
        suggestions.push(sugg);
    }
    
    // Priority 5: Absolute from origin (last resort)
    for param_id in &unset_params {
        if covered.contains(param_id) {
            continue;
        }
        
        suggestions.push(MeasurementSuggestion {
            target_parameters: vec![*param_id],
            measurement_type: MeasurementType::AbsoluteFromOrigin,
            description: format!("Coordinate p[{}] (measured from origin)", param_id),
            from_point: Some(Point::new(0.0, 0.0)),
            to_point: None,
            priority: 5,
            alternatives: vec![],
        });
    }
    
    // Sort by priority
    suggestions.sort_by_key(|s| s.priority);
    
    suggestions
}

/// Find room dimensions (distance between parallel walls)
fn find_room_dimensions(plan: &FloorPlan) -> HashMap<ParameterId, MeasurementSuggestion> {
    let mut room_dims = HashMap::new();
    let polygon = plan.polygon();
    let edges = polygon.edges();
    
    // Find pairs of parallel edges
    for i in 0..edges.len() {
        for j in (i + 1)..edges.len() {
            if are_edges_parallel(&edges[i], &edges[j]) {
                // Check if they're opposite (facing each other)
                if are_edges_opposite(&edges[i], &edges[j]) {
                    // The distance between them might be a room dimension
                    if let Some(param_id) = find_separation_parameter(plan, i, j) {
                        let distance = calculate_edge_separation(&edges[i], &edges[j]);
                        
                        room_dims.insert(param_id, MeasurementSuggestion {
                            target_parameters: vec![param_id],
                            measurement_type: MeasurementType::RoomDimension,
                            description: format!(
                                "Width between wall {} and wall {} (room dimension)",
                                get_wall_name(&polygon, i),
                                get_wall_name(&polygon, j)
                            ),
                            from_point: Some(edges[i].start),
                            to_point: Some(edges[j].start),
                            priority: 2,
                            alternatives: vec![
                                AlternativeMeasurement {
                                    description: format!("Measure wall {} directly", 
                                        get_wall_name(&polygon, i)),
                                    measurement_type: MeasurementType::EdgeLength,
                                    computation: "Use edge length".to_string(),
                                    depends_on: vec![],
                                }
                            ],
                        });
                    }
                }
            }
        }
    }
    
    room_dims
}

/// Find if a parameter can be measured incrementally from a known point
fn find_incremental_measurement(
    plan: &FloorPlan,
    param_id: ParameterId,
    known_params: &HashSet<ParameterId>,
) -> Option<MeasurementSuggestion> {
    // Look through all vertices to find ones that reference this parameter
    for (vertex_idx, vertex) in plan.parametric.vertices.iter().enumerate() {
        // Check if this parameter appears in x or y expression
        let in_x = matches!(&vertex.x_expr, CoordinateExpression::Parameter(p) if *p == param_id);
        let in_y = matches!(&vertex.y_expr, CoordinateExpression::Parameter(p) if *p == param_id);
        
        if !in_x && !in_y {
            continue;
        }
        
        // Find a nearby vertex that has a known coordinate on the same axis
        if in_x {
            // Look for a vertex with known x
            for (other_idx, other_vertex) in plan.parametric.vertices.iter().enumerate() {
                if other_idx == vertex_idx {
                    continue;
                }
                
                if let CoordinateExpression::Parameter(other_param) = &other_vertex.x_expr {
                    if known_params.contains(other_param) {
                        // Found a known reference point on the same axis
                        let polygon = plan.polygon();
                        
                        return Some(MeasurementSuggestion {
                            target_parameters: vec![param_id],
                            measurement_type: MeasurementType::IncrementalOffset,
                            description: format!(
                                "Distance from point {} to point {} (horizontal)",
                                other_idx, vertex_idx
                            ),
                            from_point: Some(polygon.vertices[other_idx]),
                            to_point: Some(polygon.vertices[vertex_idx]),
                            priority: 3,
                            alternatives: vec![
                                AlternativeMeasurement {
                                    description: format!(
                                        "Total distance from origin to point {}",
                                        vertex_idx
                                    ),
                                    measurement_type: MeasurementType::AbsoluteFromOrigin,
                                    computation: format!("p[{}] = measured_value", param_id),
                                    depends_on: vec![],
                                },
                                AlternativeMeasurement {
                                    description: format!(
                                        "Distance = measured - p[{}]",
                                        other_param
                                    ),
                                    measurement_type: MeasurementType::IncrementalOffset,
                                    computation: format!("p[{}] = measured - p[{}]", 
                                        param_id, other_param),
                                    depends_on: vec![*other_param],
                                }
                            ],
                        });
                    }
                }
            }
        }
        
        // Similar logic for y coordinates...
    }
    
    None
}

/// Find overall building dimensions
fn find_overall_dimensions(
    plan: &FloorPlan,
    unset_params: &[ParameterId],
    covered: &HashSet<ParameterId>,
) -> Vec<MeasurementSuggestion> {
    let mut suggestions = Vec::new();
    let polygon = plan.polygon();
    
    // Calculate bounding box
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    
    for vertex in &polygon.vertices {
        min_x = min_x.min(vertex.x);
        max_x = max_x.max(vertex.x);
        min_y = min_y.min(vertex.y);
        max_y = max_y.max(vertex.y);
    }
    
    // Find parameters that control the max extents
    for param_id in unset_params {
        if covered.contains(param_id) {
            continue;
        }
        
        // Check if this parameter represents an overall dimension
        if let Some(value) = plan.parametric.parameters.get(param_id) {
            if (value - max_x).abs() < 0.1 {
                suggestions.push(MeasurementSuggestion {
                    target_parameters: vec![*param_id],
                    measurement_type: MeasurementType::OverallDimension,
                    description: "Overall building width (can measure from outside)".to_string(),
                    from_point: Some(Point::new(min_x, min_y)),
                    to_point: Some(Point::new(max_x, min_y)),
                    priority: 4,
                    alternatives: vec![],
                });
            } else if (value - max_y).abs() < 0.1 {
                suggestions.push(MeasurementSuggestion {
                    target_parameters: vec![*param_id],
                    measurement_type: MeasurementType::OverallDimension,
                    description: "Overall building depth (can measure from outside)".to_string(),
                    from_point: Some(Point::new(min_x, min_y)),
                    to_point: Some(Point::new(min_x, max_y)),
                    priority: 4,
                    alternatives: vec![],
                });
            }
        }
    }
    
    suggestions
}

// Helper functions

fn are_edges_parallel(edge1: &Line, edge2: &Line) -> bool {
    let angle1 = edge1.angle();
    let angle2 = edge2.angle();
    let diff = (angle1 - angle2).abs();
    diff < 0.1 || (std::f64::consts::PI - diff).abs() < 0.1
}

fn are_edges_opposite(edge1: &Line, edge2: &Line) -> bool {
    // Check if edges face each other (not just parallel)
    let mid1 = Point::new(
        (edge1.start.x + edge1.end.x) / 2.0,
        (edge1.start.y + edge1.end.y) / 2.0,
    );
    let mid2 = Point::new(
        (edge2.start.x + edge2.end.x) / 2.0,
        (edge2.start.y + edge2.end.y) / 2.0,
    );
    
    // Simple heuristic: they should be separated along the perpendicular axis
    let dx = edge1.end.x - edge1.start.x;
    let dy = edge1.end.y - edge1.start.y;
    
    if dx.abs() > dy.abs() {
        // Horizontal edges - should be separated vertically
        (mid1.y - mid2.y).abs() > 1.0
    } else {
        // Vertical edges - should be separated horizontally
        (mid1.x - mid2.x).abs() > 1.0
    }
}

fn calculate_edge_separation(edge1: &Line, edge2: &Line) -> f64 {
    let mid1 = Point::new(
        (edge1.start.x + edge1.end.x) / 2.0,
        (edge1.start.y + edge1.end.y) / 2.0,
    );
    let mid2 = Point::new(
        (edge2.start.x + edge2.end.x) / 2.0,
        (edge2.start.y + edge2.end.y) / 2.0,
    );
    
    mid1.distance_to(&mid2)
}

fn find_separation_parameter(
    _plan: &FloorPlan,
    _edge1: usize,
    _edge2: usize,
) -> Option<ParameterId> {
    // This would analyze which parameter controls the separation
    // For now, simplified
    None
}

fn get_wall_name(_polygon: &crate::Polygon, edge_idx: usize) -> String {
    // Could be enhanced with directional names like "North wall", "East wall"
    // For now, just return the index
    format!("#{}", edge_idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stroke;
    
    #[test]
    fn test_measurement_suggestions() {
        let points = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 5.0),
            Point::new(0.0, 5.0),
        ];
        
        let stroke = Stroke::new(points);
        let plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5).unwrap();
        
        let suggestions = suggest_measurements(&plan);
        
        // Should prefer edge lengths first
        assert!(!suggestions.is_empty());
        if let Some(first) = suggestions.first() {
            assert_eq!(first.measurement_type, MeasurementType::EdgeLength);
        }
    }
}
