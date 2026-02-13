//! Dimension management and calculation

use crate::{FloorPlan, Point, Line, Error};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A dimension constraint
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Dimension {
    pub kind: DimensionKind,
    pub value: f64,
}

/// Types of dimensions
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum DimensionKind {
    /// Distance along an edge
    EdgeLength { edge_index: usize },
    
    /// Distance between two parallel edges
    Separation { edge1: usize, edge2: usize },
    
    /// Distance between two points
    PointToPoint { point1: usize, point2: usize },
    
    /// Overall width (bounding box)
    OverallWidth,
    
    /// Overall height (bounding box)
    OverallHeight,
}

/// A request for the user to provide a dimension
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DimensionRequest {
    pub kind: DimensionKind,
    pub description: String,
    pub current_value: f64,
    pub priority: DimensionPriority,
}

/// Priority of a dimension request
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum DimensionPriority {
    Critical,  // Required to fully constrain the plan
    High,      // Helpful for accuracy
    Low,       // Optional, for reference
}

/// Calculate which dimensions (edge lengths) are needed to fully constrain the floor plan
/// This should be called AFTER collinear constraints have been set
pub fn calculate_required_dimensions(plan: &FloorPlan) -> Vec<DimensionRequest> {
    let mut requests = Vec::new();
    
    let edges = plan.polygon.edges();
    let num_vertices = plan.polygon.vertex_count();
    
    // Count degrees of freedom
    // Each vertex has 2 DOF (x, y)
    let total_dof = num_vertices * 2;
    
    // Count constrained DOF
    let mut constrained_dof = 2; // Polygon closure provides 2 constraints
    
    // Count horizontal/vertical constraints (each removes 1 DOF)
    for constraint in &plan.constraints.constraints {
        match constraint {
            crate::constraints::Constraint::Horizontal { .. } => constrained_dof += 1,
            crate::constraints::Constraint::Vertical { .. } => constrained_dof += 1,
            crate::constraints::Constraint::Collinear { .. } => constrained_dof += 1,
            crate::constraints::Constraint::FixedLength { .. } => constrained_dof += 1,
            crate::constraints::Constraint::FixedVertex { .. } => constrained_dof += 2,
        }
    }
    
    let remaining_dof = total_dof.saturating_sub(constrained_dof);
    
    // Find which edges already have length constraints
    let mut has_length: Vec<bool> = vec![false; edges.len()];
    for constraint in &plan.constraints.constraints {
        if let crate::constraints::Constraint::FixedLength { edge, .. } = constraint {
            if *edge < has_length.len() {
                has_length[*edge] = true;
            }
        }
    }
    
    // Group edges by collinearity
    // Edges that are collinear only need one length between them
    let mut collinear_groups: Vec<Vec<usize>> = Vec::new();
    let mut edge_to_group: Vec<Option<usize>> = vec![None; edges.len()];
    
    for constraint in &plan.constraints.constraints {
        if let crate::constraints::Constraint::Collinear { edge1, edge2 } = constraint {
            // Find if either edge is already in a group
            let group1 = edge_to_group[*edge1];
            let group2 = edge_to_group[*edge2];
            
            match (group1, group2) {
                (Some(g1), Some(g2)) if g1 != g2 => {
                    // Merge groups
                    let group2_members = collinear_groups[g2].clone();
                    collinear_groups[g1].extend(group2_members.iter());
                    for &edge in &group2_members {
                        edge_to_group[edge] = Some(g1);
                    }
                    collinear_groups[g2].clear();
                }
                (Some(g), None) => {
                    // Add edge2 to edge1's group
                    collinear_groups[g].push(*edge2);
                    edge_to_group[*edge2] = Some(g);
                }
                (None, Some(g)) => {
                    // Add edge1 to edge2's group
                    collinear_groups[g].push(*edge1);
                    edge_to_group[*edge1] = Some(g);
                }
                (None, None) => {
                    // Create new group
                    let group_idx = collinear_groups.len();
                    collinear_groups.push(vec![*edge1, *edge2]);
                    edge_to_group[*edge1] = Some(group_idx);
                    edge_to_group[*edge2] = Some(group_idx);
                }
                _ => {}
            }
        }
    }
    
    // For each collinear group, we only need one dimension
    let mut group_has_dimension = vec![false; collinear_groups.len()];
    for (edge_idx, has_len) in has_length.iter().enumerate() {
        if *has_len {
            if let Some(group_idx) = edge_to_group[edge_idx] {
                group_has_dimension[group_idx] = true;
            }
        }
    }
    
    // Request dimensions for edges that don't have them
    let mut requested_count = 0;
    
    // First pass: one dimension per collinear group
    for (group_idx, group) in collinear_groups.iter().enumerate() {
        if group.is_empty() || group_has_dimension[group_idx] {
            continue;
        }
        
        // Pick the first edge in the group to request
        let edge_idx = group[0];
        let edge = &edges[edge_idx];
        
        let priority = if requested_count < remaining_dof {
            DimensionPriority::Critical
        } else {
            DimensionPriority::High
        };
        
        requests.push(DimensionRequest {
            kind: DimensionKind::EdgeLength { edge_index: edge_idx },
            description: format!("Length of edge {} (represents {} collinear edges)", 
                edge_idx, group.len()),
            current_value: edge.length(),
            priority,
        });
        
        requested_count += 1;
    }
    
    // Second pass: standalone edges (not in any collinear group)
    for edge_idx in 0..edges.len() {
        if has_length[edge_idx] || edge_to_group[edge_idx].is_some() {
            continue;
        }
        
        let edge = &edges[edge_idx];
        
        let priority = if requested_count < remaining_dof {
            DimensionPriority::Critical
        } else {
            DimensionPriority::High
        };
        
        requests.push(DimensionRequest {
            kind: DimensionKind::EdgeLength { edge_index: edge_idx },
            description: format!("Length of edge {}", edge_idx),
            current_value: edge.length(),
            priority,
        });
        
        requested_count += 1;
        
        if requested_count >= remaining_dof + 2 {
            break; // Don't suggest too many dimensions
        }
    }
    
    // Always suggest overall dimensions as reference (low priority)
    let bounds = calculate_bounds(plan);
    if let Some((min, max)) = bounds {
        requests.push(DimensionRequest {
            kind: DimensionKind::OverallWidth,
            description: "Overall width of the plan".to_string(),
            current_value: max.x - min.x,
            priority: DimensionPriority::Low,
        });
        
        requests.push(DimensionRequest {
            kind: DimensionKind::OverallHeight,
            description: "Overall height of the plan".to_string(),
            current_value: max.y - min.y,
            priority: DimensionPriority::Low,
        });
    }
    
    // Sort by priority (Critical first, then High, then Low)
    requests.sort_by(|a, b| a.priority.cmp(&b.priority));
    
    requests
}

/// Apply a dimension to the floor plan
pub fn apply_dimension(plan: &mut FloorPlan, dimension: Dimension) -> Result<(), Error> {
    match dimension.kind {
        DimensionKind::EdgeLength { edge_index } => {
            plan.constraints.add_fixed_length(edge_index, dimension.value)?;
            plan.apply_constraints()?;
        }
        
        DimensionKind::OverallWidth => {
            // Scale the plan to match the desired width
            let bounds = calculate_bounds(plan).ok_or_else(|| {
                Error::DimensionError("Cannot calculate bounds".to_string())
            })?;
            
            let current_width = bounds.1.x - bounds.0.x;
            if current_width < 1e-10 {
                return Err(Error::DimensionError("Zero width polygon".to_string()));
            }
            
            let scale = dimension.value / current_width;
            scale_plan(plan, scale, 1.0);
        }
        
        DimensionKind::OverallHeight => {
            // Scale the plan to match the desired height
            let bounds = calculate_bounds(plan).ok_or_else(|| {
                Error::DimensionError("Cannot calculate bounds".to_string())
            })?;
            
            let current_height = bounds.1.y - bounds.0.y;
            if current_height < 1e-10 {
                return Err(Error::DimensionError("Zero height polygon".to_string()));
            }
            
            let scale = dimension.value / current_height;
            scale_plan(plan, 1.0, scale);
        }
        
        DimensionKind::Separation { edge1, edge2 } => {
            // This is more complex and would need constraint solving
            return Err(Error::DimensionError(
                "Separation dimensions not yet implemented".to_string()
            ));
        }
        
        DimensionKind::PointToPoint { point1, point2 } => {
            // Calculate current distance and scale
            if point1 >= plan.polygon.vertices.len() || point2 >= plan.polygon.vertices.len() {
                return Err(Error::InvalidEdgeIndex(point1.max(point2)));
            }
            
            let p1 = plan.polygon.vertices[point1];
            let p2 = plan.polygon.vertices[point2];
            let current_distance = p1.distance_to(&p2);
            
            if current_distance < 1e-10 {
                return Err(Error::DimensionError("Zero distance between points".to_string()));
            }
            
            let scale = dimension.value / current_distance;
            scale_plan(plan, scale, scale);
        }
    }
    
    Ok(())
}

/// Calculate the bounding box of the floor plan
fn calculate_bounds(plan: &FloorPlan) -> Option<(Point, Point)> {
    if plan.polygon.vertices.is_empty() {
        return None;
    }
    
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    
    for vertex in &plan.polygon.vertices {
        min_x = min_x.min(vertex.x);
        min_y = min_y.min(vertex.y);
        max_x = max_x.max(vertex.x);
        max_y = max_y.max(vertex.y);
    }
    
    Some((Point::new(min_x, min_y), Point::new(max_x, max_y)))
}

/// Scale the entire floor plan
fn scale_plan(plan: &mut FloorPlan, scale_x: f64, scale_y: f64) {
    // Calculate center point
    if let Some((min, max)) = calculate_bounds(plan) {
        let center_x = (min.x + max.x) / 2.0;
        let center_y = (min.y + max.y) / 2.0;
        
        // Scale vertices around center
        for vertex in &mut plan.polygon.vertices {
            vertex.x = center_x + (vertex.x - center_x) * scale_x;
            vertex.y = center_y + (vertex.y - center_y) * scale_y;
        }
    }
}

/// Get the current value of a dimension
pub fn get_dimension_value(plan: &FloorPlan, kind: &DimensionKind) -> Result<f64, Error> {
    match kind {
        DimensionKind::EdgeLength { edge_index } => {
            let edge = plan.polygon.edge(*edge_index)
                .ok_or_else(|| Error::InvalidEdgeIndex(*edge_index))?;
            Ok(edge.length())
        }
        
        DimensionKind::OverallWidth => {
            let bounds = calculate_bounds(plan)
                .ok_or_else(|| Error::DimensionError("Cannot calculate bounds".to_string()))?;
            Ok(bounds.1.x - bounds.0.x)
        }
        
        DimensionKind::OverallHeight => {
            let bounds = calculate_bounds(plan)
                .ok_or_else(|| Error::DimensionError("Cannot calculate bounds".to_string()))?;
            Ok(bounds.1.y - bounds.0.y)
        }
        
        DimensionKind::PointToPoint { point1, point2 } => {
            if *point1 >= plan.polygon.vertices.len() {
                return Err(Error::InvalidEdgeIndex(*point1));
            }
            if *point2 >= plan.polygon.vertices.len() {
                return Err(Error::InvalidEdgeIndex(*point2));
            }
            
            let p1 = plan.polygon.vertices[*point1];
            let p2 = plan.polygon.vertices[*point2];
            Ok(p1.distance_to(&p2))
        }
        
        DimensionKind::Separation { .. } => {
            Err(Error::DimensionError("Separation dimensions not yet implemented".to_string()))
        }
    }
}
