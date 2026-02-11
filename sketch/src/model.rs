// ===== sketch/src/model.rs =====

pub type Point2f = nalgebra::Point2<f32>;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DrawMode {
    #[default]
    FloorPlan,
    Room,
    Wall,
    Door,
    Window,
    Eraser,
}

#[derive(Debug, Clone, Default)]
pub struct Stroke {
    pub points: Vec<Point2f>,
    pub mode: DrawMode,
    pub is_closed: bool,
}

impl Stroke {
    pub fn new(mode: DrawMode, start: Point2f) -> Self {
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
