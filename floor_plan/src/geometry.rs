use nalgebra::{Point2, Point3};

// Helper function to check if a point lies on a line segment in 2D
pub fn point_on_segment_2d(
    p: &Point2<f64>,
    a: &Point3<f64>,
    b: &Point3<f64>,
    epsilon: f64,
) -> bool {
    // Check if point is collinear with the segment
    let cross = (p.y - a.y) * (b.x - a.x) - (p.x - a.x) * (b.y - a.y);
    if cross.abs() > epsilon {
        return false;
    }

    // Check if point is between a and b
    let dot = (p.x - a.x) * (b.x - a.x) + (p.y - a.y) * (b.y - a.y);
    let len_sq = (b.x - a.x) * (b.x - a.x) + (b.y - a.y) * (b.y - a.y);

    dot >= -epsilon && dot <= len_sq + epsilon
}

// Helper function to check if a point is inside a triangle in 2D using barycentric coordinates
pub fn point_in_triangle_2d(
    p: &Point2<f64>,
    a: &Point2<f64>,
    b: &Point2<f64>,
    c: &Point2<f64>,
    epsilon: f64,
) -> bool {
    let v0x = c.x - a.x;
    let v0y = c.y - a.y;
    let v1x = b.x - a.x;
    let v1y = b.y - a.y;
    let v2x = p.x - a.x;
    let v2y = p.y - a.y;

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
