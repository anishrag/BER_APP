// ===== sketch/src/engine.rs =====
//
use crate::model::Stroke;
use domain::sketch::SketchMode;

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
    pub mode: SketchMode,
    pub close_epsilon: f32,
}

impl SketchEngine {
    pub fn new() -> Self {
        Self {
            stroke: None,
            mode: SketchMode::Storey2D,
            close_epsilon: 8.0,
        }
    }

    pub fn set_mode(&mut self, mode: SketchMode) {
        self.mode = mode;
    }
}
