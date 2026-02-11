// ===== sketch/src/engine.rs =====
use crate::model::{DrawMode, Point2f, Stroke};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SketchEvent {
    StrokeStarted,
    StrokeUpdated,
    StrokeFinished { stroke_idx: usize },
    StrokeClosed { stroke_idx: usize },
}

#[derive(Debug, Default)]
pub struct SketchEngine {
    pub stroke: Option<Stroke>,
    pub mode: DrawMode,
    pub close_epsilon: f32,
}

impl SketchEngine {
    pub fn new() -> Self {
        Self {
            stroke: None,
            mode: DrawMode::FloorPlan,
            close_epsilon: 8.0,
        }
    }

    pub fn set_mode(&mut self, mode: DrawMode) {
        self.mode = mode;
    }
}
