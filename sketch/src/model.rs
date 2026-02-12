// ===== sketch/src/model.rs =====
use domain::sketch::SketchMode;


pub type Point2f = nalgebra::Point2<f32>;

#[derive(Debug, Clone, Default)]
pub struct Stroke {
    pub points: Vec<Point2f>,
    pub mode: SketchMode,
    pub is_closed: bool,
}

impl Stroke {
    pub fn new(mode: SketchMode, start: Point2f) -> Self {
        Self {
            points: vec![start],
            mode,
            is_closed: false,
        }
    }
}

#[derive(Default)]
pub struct SketchState {
    // Add any sketch-specific state here if needed
    pub current: Vec<Stroke>,
}
