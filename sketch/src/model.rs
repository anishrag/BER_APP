// ===== sketch/src/model.rs =====
use domain::sketch::EditTools;

pub type Point2f = nalgebra::Point2<f32>;

#[derive(Debug, Clone, Default)]
pub struct Stroke {
    pub points: Vec<Point2f>,
    pub mode: EditTools,
    pub is_closed: bool,
}
impl Stroke {
    pub fn new(mode: EditTools, start: Point2f) -> Self {
        Self {
            points: vec![start],
            mode,
            is_closed: false,
        }
    }

    pub fn first_point(&self) -> Point2f {
        // Safe because Stroke::new always inserts one point
        *self.points.first().expect("Stroke has no points")
    }

    pub fn last_point(&self) -> Point2f {
        *self.points.last().expect("Stroke has no points")
    }

    pub fn insert_at_start(&mut self, p: Point2f) {
        self.points.insert(0, p);
    }

    pub fn push(&mut self, p: Point2f) {
        self.points.push(p);
    }
}
