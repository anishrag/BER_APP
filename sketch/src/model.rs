// ===== sketch/src/model.rs =====
use domain::points::Point2f32;
use domain::sketch::EditTools;

#[derive(Debug, Clone, Default)]
pub struct Stroke {
    pub points: Vec<Point2f32>,
    pub mode: EditTools,
    pub is_closed: bool,
}
impl Stroke {
    pub fn new(mode: EditTools, start: Point2f32) -> Self {
        Self {
            points: vec![start],
            mode,
            is_closed: false,
        }
    }

    pub fn first_point(&self) -> Point2f32 {
        // Safe because Stroke::new always inserts one point
        *self.points.first().expect("Stroke has no points")
    }

    pub fn last_point(&self) -> Point2f32 {
        *self.points.last().expect("Stroke has no points")
    }

    pub fn insert_at_start(&mut self, p: Point2f32) {
        self.points.insert(0, p);
    }

    pub fn push(&mut self, p: Point2f32) {
        self.points.push(p);
    }
}
