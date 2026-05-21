// ===== sketch/src/engine.rs =====
//
use crate::fresh_2d_perp::Storey2DPerpWalls;
use crate::model::Stroke;
use domain::points::Point2f32;
use domain::sketch::{DisplayMode, EditTools, SketchMode};

/// Distance (px) within which lifting the mouse near the stroke's start point
/// auto-closes the polygon.
pub const DEFAULT_CLOSE_EPSILON: f32 = 30.0;

/// Distance (px) within which a new touch near an existing stroke's endpoint
/// extends that stroke instead of starting a fresh one. Should be > [`DEFAULT_CLOSE_EPSILON`].
pub const DEFAULT_SNAP_EPSILON: f32 = 100.0;
pub trait SketchModeBehaviour: std::fmt::Debug {
    fn start_stroke(
        &mut self,
        stroke: &mut Option<Stroke>,
        edit_tool: EditTools,
        close_epsilon: f32,
        snap_epsilon: f32,
        append_mode: &mut StrokeAppendMode,
        start: Point2f32,
    ) -> Result<(), SketchError>;
    fn add_point(
        &mut self,
        stroke: &mut Option<Stroke>,
        append_mode: StrokeAppendMode,
        point: Point2f32,
    ) -> Result<(), SketchError>;
    fn end_stroke(
        &mut self,
        stroke: &mut Option<Stroke>,
        close_epsilon: f32,
    ) -> Result<Option<Stroke>, SketchError>;
}

#[derive(Debug)]
pub enum SketchError {
    NoActiveStroke,
    SnapTooFar,
    StrokeAlreadyClosed,
    InvalidStroke,
    ProcessingError,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SketchEvent {
    StrokeStarted,
    StrokeUpdated,
    StrokeFinished { stroke_idx: usize },
    StrokeClosed { stroke_idx: usize },
}

#[derive(Default, Debug, Clone)]
pub enum StrokeAppendMode {
    #[default]
    Forward, // push to end
    Backward, // insert at start
}

#[derive(Debug)]
pub struct SketchEngine {
    pub stroke: Option<Stroke>,
    pub mode_behaviour: Box<dyn SketchModeBehaviour>,
    pub mode: SketchMode,
    pub display: DisplayMode,
    pub edit_tool: EditTools,
    pub close_epsilon: f32,
    pub snap_epsilon: f32,
    pub append_mode: StrokeAppendMode,
}

impl Default for SketchEngine {
    fn default() -> Self {
        Self {
            stroke: None,
            mode_behaviour: Box::new(Storey2DPerpWalls),
            mode: SketchMode::Storey2D,
            display: DisplayMode::FloorMapDims,
            edit_tool: EditTools::DrawPerpWalls,
            close_epsilon: DEFAULT_CLOSE_EPSILON,
            snap_epsilon: DEFAULT_SNAP_EPSILON,
            append_mode: StrokeAppendMode::Forward,
        }
    }
}

impl SketchEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_mode_behaviour(&mut self, behaviour: Box<dyn SketchModeBehaviour>) {
        self.mode_behaviour = behaviour;
    }
    pub fn set_mode(&mut self, mode: SketchMode) {
        self.mode = mode;
        self.update_mode_behaviour();
    }
    pub fn set_display(&mut self, mode: DisplayMode) {
        self.display = mode;
        self.update_mode_behaviour();
    }
    pub fn set_edit_tools(&mut self, mode: EditTools) {
        self.edit_tool = mode;
        self.update_mode_behaviour();
    }

    fn update_mode_behaviour(&mut self) {
        // Here you would match on self.mode and set the appropriate behaviour
        // For example:
        match (self.mode, self.edit_tool) {
            // DrawPerpWalls and AddSection share the same stroke-capture
            // behaviour — both draw an orthogonal polygon. The controller
            // decides on stroke completion whether to replace the primary
            // outline or append a new ring.
            (SketchMode::Storey2D, EditTools::DrawPerpWalls)
            | (SketchMode::Storey2D, EditTools::AddSection) => {
                self.set_mode_behaviour(Box::new(Storey2DPerpWalls));
            }
            _ => (),
        }
    }
    pub fn start_stroke(&mut self, start: Point2f32) -> Result<(), SketchError> {
        self.mode_behaviour.start_stroke(
            &mut self.stroke,
            self.edit_tool,
            self.close_epsilon,
            self.snap_epsilon,
            &mut self.append_mode,
            start,
        )
    }

    pub fn add_point(&mut self, point: Point2f32) -> Result<(), SketchError> {
        self.mode_behaviour
            .add_point(&mut self.stroke, self.append_mode.clone(), point)
    }

    pub fn end_stroke(&mut self) -> Result<Option<Stroke>, SketchError> {
        self.mode_behaviour
            .end_stroke(&mut self.stroke, self.close_epsilon)
    }
}
