use geo::{LineString, Polygon};

use super::types::{Polygon3D, Vertex3D};

// Helper function to check if a point lies on a line segment in 2D
fn point_on_segment_2d(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64, epsilon: f64) -> bool {
    // Check if point is collinear with the segment
    let cross = (py - ay) * (bx - ax) - (px - ax) * (by - ay);
    if cross.abs() > epsilon {
        return false;
    }

    // Check if point is between a and b
    let dot = (px - ax) * (bx - ax) + (py - ay) * (by - ay);
    let len_sq = (bx - ax) * (bx - ax) + (by - ay) * (by - ay);

    dot >= -epsilon && dot <= len_sq + epsilon
}

// Helper function to check if a point is inside a triangle in 2D using barycentric coordinates
fn point_in_triangle_2d(
    px: f64,
    py: f64,
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
    cx: f64,
    cy: f64,
    epsilon: f64,
) -> bool {
    let v0x = cx - ax;
    let v0y = cy - ay;
    let v1x = bx - ax;
    let v1y = by - ay;
    let v2x = px - ax;
    let v2y = py - ay;

    let dot00 = v0x * v0x + v0y * v0y;
    let dot01 = v0x * v1x + v0y * v1y;
    let dot02 = v0x * v2x + v0y * v2y;
    let dot11 = v1x * v1x + v1y * v1y;
    let dot12 = v1x * v2x + v1y * v2y;

    let inv_denom = 1.0 / (dot00 * dot11 - dot01 * dot01);
    let u = (dot11 * dot02 - dot01 * dot12) * inv_denom;
    let v = (dot00 * dot12 - dot01 * dot02) * inv_denom;

    (u >= -epsilon) && (v >= -epsilon) && (u + v <= 1.0 + epsilon)
}

fn polygon3d_to_geo(poly: &Polygon3D) -> Polygon<f64> {
    let coords = poly.exterior.iter().map(|v| (v.x, v.y)).collect::<Vec<_>>();

    Polygon::new(LineString::from(coords), vec![])
}

fn geo_to_polygon3d(poly: &Polygon<f64>, z: f64) -> Polygon3D {
    let exterior = poly
        .exterior()
        .points_iter()
        .map(|p| Vertex3D {
            x: p.x(),
            y: p.y(),
            z,
        })
        .collect();

    Polygon3D { exterior }
}
