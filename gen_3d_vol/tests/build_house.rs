use domain::points::Point2f64;
use gen_3d_vol::{FloorConstructionType, Gen3D};
#[test]
fn test_my_house_synthetic() {
    let mut my_house_3d = Gen3D::new_floor(
        vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, -1.0),
            Point2f64::new(6.0, -1.0),
            Point2f64::new(8.0, 1.0),
            Point2f64::new(8.0, 14.0),
            Point2f64::new(1.0, 14.0),
            Point2f64::new(1.0, 8.0),
            Point2f64::new(0.0, 8.0),
        ],
        0.0,
        FloorConstructionType::Uninitialised,
        None,
    );
    my_house_3d.add_ceiling_with_height(2.5);
    my_house_3d.raise_lower_ceiling_split(
        vec![
            Point2f64::new(1.0, 8.0),
            Point2f64::new(1.0, 10.0),
            Point2f64::new(5.0, 10.0),
            Point2f64::new(5.0, 8.0),
        ],
        1.5,
    );
    my_house_3d.raise_lower_ceiling_split(
        vec![
            Point2f64::new(1.0, 10.0),
            Point2f64::new(1.0, 14.0),
            Point2f64::new(5.0, 14.0),
            Point2f64::new(5.0, 10.0),
        ],
        1.3,
    );
}
