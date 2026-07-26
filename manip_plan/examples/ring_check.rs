//! Dev check: does FloorPlan::from_clean_ring accept a given ring?
use domain::points::Point2f32;
fn main() {
    let ring = [
        (5.0309648513793945, 24.431068420410156),
        (5.436964988708496, 25.007068634033203),
        (6.326964855194092, 25.007068634033203),
        (6.886964797973633, 24.508169174194336),
        (6.886964797973633, 23.08106803894043),
        (5.0309648513793945, 23.08106803894043),
    ];
    let pts: Vec<Point2f32> = ring
        .iter()
        .map(|&(x, y)| Point2f32::new(x as f32, y as f32))
        .collect();
    match manip_plan::FloorPlan::from_clean_ring(&pts) {
        Ok(p) => println!(
            "OK — {} points, params {}",
            p.get_points().len(),
            p.all_active_params().len()
        ),
        Err(e) => println!("FAILED: {e:?}"),
    }
    // Reversed orientation, in case order matters.
    let rev: Vec<Point2f32> = pts.iter().rev().copied().collect();
    match manip_plan::FloorPlan::from_clean_ring(&rev) {
        Ok(_) => println!("reversed: OK"),
        Err(e) => println!("reversed FAILED: {e:?}"),
    }
}
