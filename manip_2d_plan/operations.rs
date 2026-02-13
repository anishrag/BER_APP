//! Geometric operations for adding and trimming areas

use crate::{FloorPlan, Stroke, Polygon, Line, Point, Error};
use crate::simplification::{simplify_stroke, simplify_to_polygon};
use crate::orthogonal::make_orthogonal;
use crate::snapping::{snap_to_polygon, snap_stroke_to_polygon};
use geo::{BooleanOps, Area};
use geo_types::MultiPolygon;

/// Add an area to the floor plan
pub fn add_area(plan: &FloorPlan, stroke: &Stroke, orthogonal: bool) -> Result<FloorPlan, Error> {
    // Simplify the stroke
    let tolerance = 1.0;
    let new_area = if orthogonal {
        let simplified = simplify_stroke(stroke, tolerance)?;
        make_orthogonal(&simplified, tolerance)?
    } else {
        simplify_to_polygon(stroke, tolerance)?
    };
    
    // Snap the new area to the existing plan if close enough
    let snap_distance = 5.0;
    let snapped_vertices = snap_stroke_to_polygon(&new_area.vertices, &plan.polygon, snap_distance);
    let snapped_area = Polygon::new(snapped_vertices);
    
    // Perform union operation
    let existing_geo = plan.polygon.to_geo_polygon();
    let new_geo = snapped_area.to_geo_polygon();
    
    let result = existing_geo.union(&new_geo);
    
    // Convert result back to our polygon type
    let updated_polygon = match result {
        MultiPolygon(polygons) => {
            if polygons.is_empty() {
                return Err(Error::BooleanOpError("Union resulted in empty geometry".to_string()));
            }
            // Take the largest polygon if there are multiple
            let largest = polygons.iter()
                .max_by(|a, b| a.unsigned_area().partial_cmp(&b.unsigned_area()).unwrap())
                .unwrap();
            Polygon::from_geo_polygon(largest)
        }
    };
    
    let mut new_plan = FloorPlan {
        polygon: updated_polygon,
        constraints: plan.constraints.clone(),
        rooms: Vec::new(), // Rooms need to be redetected
    };
    
    // Reapply constraints
    new_plan.apply_constraints()?;
    
    Ok(new_plan)
}

/// Trim an area from the floor plan
pub fn trim_area(plan: &FloorPlan, stroke: &Stroke, orthogonal: bool) -> Result<FloorPlan, Error> {
    // Simplify the stroke to create the trim area
    let tolerance = 1.0;
    let trim_area = if orthogonal {
        let simplified = simplify_stroke(stroke, tolerance)?;
        make_orthogonal(&simplified, tolerance)?
    } else {
        simplify_to_polygon(stroke, tolerance)?
    };
    
    // Snap to existing geometry
    let snap_distance = 5.0;
    let snapped_vertices = snap_stroke_to_polygon(&trim_area.vertices, &plan.polygon, snap_distance);
    let snapped_area = Polygon::new(snapped_vertices);
    
    // Perform difference operation
    let existing_geo = plan.polygon.to_geo_polygon();
    let trim_geo = snapped_area.to_geo_polygon();
    
    let result = existing_geo.difference(&trim_geo);
    
    // Convert result back
    let updated_polygon = match result {
        MultiPolygon(polygons) => {
            if polygons.is_empty() {
                return Err(Error::BooleanOpError("Trim resulted in empty geometry".to_string()));
            }
            // Take the largest polygon
            let largest = polygons.iter()
                .max_by(|a, b| a.unsigned_area().partial_cmp(&b.unsigned_area()).unwrap())
                .unwrap();
            Polygon::from_geo_polygon(largest)
        }
    };
    
    let mut new_plan = FloorPlan {
        polygon: updated_polygon,
        constraints: plan.constraints.clone(),
        rooms: Vec::new(),
    };
    
    new_plan.apply_constraints()?;
    
    Ok(new_plan)
}

/// Trim the floor plan by a line, keeping the largest part
pub fn trim_by_line(plan: &FloorPlan, line: &Line) -> Result<FloorPlan, Error> {
    // Create a very large rectangle perpendicular to the line
    // This will cut the polygon in half
    
    let line_length = line.length();
    if line_length < 1e-10 {
        return Err(Error::BooleanOpError("Line is too short".to_string()));
    }
    
    // Get perpendicular direction
    let dx = line.end.x - line.start.x;
    let dy = line.end.y - line.start.y;
    let perp_x = -dy / line_length;
    let perp_y = dx / line_length;
    
    // Create a cutting polygon (very large to ensure it cuts through everything)
    let huge = 10000.0;
    let cut_polygon = Polygon::new(vec![
        Point::new(line.start.x - perp_x * huge, line.start.y - perp_y * huge),
        Point::new(line.end.x - perp_x * huge, line.end.y - perp_y * huge),
        Point::new(line.end.x + perp_x * huge, line.end.y + perp_y * huge),
        Point::new(line.start.x + perp_x * huge, line.start.y + perp_y * huge),
    ]);
    
    // Perform difference
    let existing_geo = plan.polygon.to_geo_polygon();
    let cut_geo = cut_polygon.to_geo_polygon();
    
    let result = existing_geo.difference(&cut_geo);
    
    // Take the largest resulting polygon
    let updated_polygon = match result {
        MultiPolygon(polygons) => {
            if polygons.is_empty() {
                return Err(Error::BooleanOpError("Trim by line resulted in empty geometry".to_string()));
            }
            let largest = polygons.iter()
                .max_by(|a, b| a.unsigned_area().partial_cmp(&b.unsigned_area()).unwrap())
                .unwrap();
            Polygon::from_geo_polygon(largest)
        }
    };
    
    let mut new_plan = FloorPlan {
        polygon: updated_polygon,
        constraints: plan.constraints.clone(),
        rooms: Vec::new(),
    };
    
    new_plan.apply_constraints()?;
    
    Ok(new_plan)
}

/// Shift a wall (edge) while maintaining connections
pub fn shift_wall(plan: &mut FloorPlan, edge_index: usize, offset: f64) -> Result<(), Error> {
    let edge = plan.polygon.edge(edge_index)
        .ok_or_else(|| Error::InvalidEdgeIndex(edge_index))?;
    
    // Calculate perpendicular direction
    let dx = edge.end.x - edge.start.x;
    let dy = edge.end.y - edge.start.y;
    let length = edge.length();
    
    if length < 1e-10 {
        return Err(Error::GeometryError("Edge has zero length".to_string()));
    }
    
    let perp_x = -dy / length * offset;
    let perp_y = dx / length * offset;
    
    // Move the vertices of this edge
    let next_index = (edge_index + 1) % plan.polygon.vertices.len();
    plan.polygon.vertices[edge_index].x += perp_x;
    plan.polygon.vertices[edge_index].y += perp_y;
    plan.polygon.vertices[next_index].x += perp_x;
    plan.polygon.vertices[next_index].y += perp_y;
    
    // Adjacent edges need to be extended/shortened to maintain connections
    let prev_index = if edge_index == 0 {
        plan.polygon.vertices.len() - 1
    } else {
        edge_index - 1
    };
    let next_next_index = (next_index + 1) % plan.polygon.vertices.len();
    
    // Extend previous edge
    if let Some(intersection) = calculate_line_intersection(
        &plan.polygon.vertices[prev_index],
        &plan.polygon.vertices[edge_index],
        &edge,
    ) {
        plan.polygon.vertices[edge_index] = intersection;
    }
    
    // Extend next edge
    let next_edge_start = plan.polygon.vertices[next_index];
    let next_edge_end = plan.polygon.vertices[next_next_index];
    if let Some(intersection) = calculate_line_intersection(
        &next_edge_start,
        &next_edge_end,
        &edge,
    ) {
        plan.polygon.vertices[next_index] = intersection;
    }
    
    // Reapply constraints
    plan.apply_constraints()?;
    
    Ok(())
}

/// Calculate intersection of two infinite lines
fn calculate_line_intersection(p1: &Point, p2: &Point, line: &Line) -> Option<Point> {
    let x1 = p1.x;
    let y1 = p1.y;
    let x2 = p2.x;
    let y2 = p2.y;
    
    let x3 = line.start.x;
    let y3 = line.start.y;
    let x4 = line.end.x;
    let y4 = line.end.y;
    
    let denom = (x1 - x2) * (y3 - y4) - (y1 - y2) * (x3 - x4);
    
    if denom.abs() < 1e-10 {
        return None; // Lines are parallel
    }
    
    let t = ((x1 - x3) * (y3 - y4) - (y1 - y3) * (x3 - x4)) / denom;
    
    Some(Point::new(
        x1 + t * (x2 - x1),
        y1 + t * (y2 - y1),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_shift_wall() {
        // Create a simple square
        let polygon = Polygon::new(vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ]);
        
        let mut plan = FloorPlan {
            polygon,
            constraints: crate::constraints::ConstraintSet::new(),
            rooms: Vec::new(),
        };
        
        // Shift the bottom edge up by 2 units
        shift_wall(&mut plan, 0, 2.0).unwrap();
        
        // Check that the bottom edge moved
        assert!((plan.polygon.vertices[0].y - 2.0).abs() < 0.1);
        assert!((plan.polygon.vertices[1].y - 2.0).abs() < 0.1);
    }
}
