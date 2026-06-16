// ===== sketch/src/engine.rs =====
//
use crate::fresh_2d_perp::Storey2DPerpWalls;
use crate::model::Stroke;
use domain::points::Point2f32;
use domain::sketch::{DisplayMode, EditTools, SketchMode};

/// Distance (px) within which lifting the mouse near the stroke's start point
/// auto-closes the polygon.
pub(crate) const DEFAULT_CLOSE_EPSILON: f32 = 30.0;

/// Distance (px) within which a new touch near an existing stroke's endpoint
/// extends that stroke instead of starting a fresh one. Should be > [`DEFAULT_CLOSE_EPSILON`].
pub(crate) const DEFAULT_SNAP_EPSILON: f32 = 100.0;
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

#[derive(Default, Debug, Clone)]
pub enum StrokeAppendMode {
    #[default]
    Forward, // push to end
    Backward, // insert at start
}

#[derive(Debug)]
pub struct SketchEngine {
    pub stroke: Option<Stroke>,
    pub(crate) mode_behaviour: Box<dyn SketchModeBehaviour>,
    pub mode: SketchMode,
    pub display: DisplayMode,
    pub edit_tool: EditTools,
    pub(crate) close_epsilon: f32,
    pub(crate) snap_epsilon: f32,
    pub(crate) append_mode: StrokeAppendMode,
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

    pub(crate) fn set_mode_behaviour(&mut self, behaviour: Box<dyn SketchModeBehaviour>) {
        self.mode_behaviour = behaviour;
    }
    pub fn set_mode(&mut self, mode: SketchMode) {
        self.mode = mode;
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
            // DrawPerpWalls, AddDetachedSection, Extend, and Trim all share the
            // same freehand stroke-capture behaviour. The controller decides on
            // stroke completion what to do: replace the primary outline, append
            // a detached ring, merge an extend porch, or cut along a trim line.
            (SketchMode::Storey2D, EditTools::DrawPerpWalls)
            | (SketchMode::Storey2D, EditTools::AddDetachedSection)
            | (SketchMode::Storey2D, EditTools::ExtendOrtho)
            | (SketchMode::Storey2D, EditTools::ExtendCurve)
            | (SketchMode::Storey2D, EditTools::Trim)
            | (SketchMode::Storey2D, EditTools::TrimOrtho)
            | (SketchMode::Storey2D, EditTools::TrimCurve) => {
                self.set_mode_behaviour(Box::new(Storey2DPerpWalls));
            }
            // Move mode doesn't capture freehand strokes — interaction
            // happens via click/drag handled at the UI layer.
            (SketchMode::Storey2D, EditTools::Move) => {
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

    /// Abandon the in-flight stroke, if any — the capture turned out to be a
    /// navigation gesture, not a drawing. No-op when none is active.
    pub fn cancel_stroke(&mut self) {
        self.stroke = None;
    }

    /// Take the current stroke out of the engine without running the
    /// "did the user close the loop?" check. Used by the Extend tool,
    /// whose strokes start and end at *different* points on an existing
    /// polygon — they're not supposed to self-close.
    pub fn take_stroke_raw(&mut self) -> Option<Stroke> {
        self.stroke.take()
    }
}
