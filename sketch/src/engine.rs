// ===== sketch/src/engine.rs =====
//
use crate::geometry;
use crate::model::{Point2f, Stroke};
use domain::sketch::{DisplayMode, EditTools, SketchMode};

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

#[derive(Default, Debug)]
pub enum StrokeAppendMode {
    #[default]
    Forward, // push to end
    Backward, // insert at start
}

#[derive(Debug, Default)]
pub struct SketchEngine {
    pub stroke: Option<Stroke>,
    pub mode: SketchMode,
    pub display: DisplayMode,
    pub edit_tool: EditTools,
    pub close_epsilon: f32,
    pub snap_epsilon: f32,
    pub append_mode: StrokeAppendMode,
}

impl SketchEngine {
    pub fn new() -> Self {
        Self {
            stroke: None,
            mode: SketchMode::Storey2D,
            display: DisplayMode::FloorMapDims,
            edit_tool: EditTools::DrawPerpWalls,
            close_epsilon: 8.0,
            snap_epsilon: 100.0,
            append_mode: StrokeAppendMode::Forward,
        }
    }

    pub fn set_mode(&mut self, mode: SketchMode) {
        self.mode = mode;
    }
    pub fn set_display(&mut self, mode: DisplayMode) {
        self.display = mode;
    }
    pub fn set_edit_tools(&mut self, mode: EditTools) {
        self.edit_tool = mode;
    }

    pub fn start_stroke(&mut self, start: Point2f) -> Result<(), SketchError> {
        match self.stroke.as_mut() {
            // --------------------------------
            // No existing stroke → create new
            // --------------------------------
            None => {
                self.stroke = Some(Stroke::new(self.edit_tool, start));
                self.append_mode = StrokeAppendMode::Forward;
                Ok(())
            }

            // --------------------------------
            // Existing stroke → attempt snap
            // --------------------------------
            Some(stroke) => {
                if stroke.check_closed(self.close_epsilon) {
                    return Err(SketchError::StrokeAlreadyClosed);
                }

                let first = stroke.first_point();
                let last = stroke.last_point();

                let dist_to_start = (first - start).norm();
                let dist_to_end = (last - start).norm();

                let can_snap_start = dist_to_start <= self.snap_epsilon;
                let can_snap_end = dist_to_end <= self.snap_epsilon;

                match (can_snap_start, can_snap_end) {
                    (false, false) => {
                        // Too far from both ends → reject
                        Err(SketchError::SnapTooFar)
                    }

                    (true, false) => {
                        self.append_mode = StrokeAppendMode::Backward;
                        stroke.insert_at_start(start);
                        Ok(())
                    }

                    (false, true) => {
                        self.append_mode = StrokeAppendMode::Forward;
                        stroke.push(start);
                        Ok(())
                    }

                    (true, true) => {
                        // Rare case: very small stroke, both ends close
                        if dist_to_start < dist_to_end {
                            self.append_mode = StrokeAppendMode::Backward;
                            stroke.insert_at_start(start);
                        } else {
                            self.append_mode = StrokeAppendMode::Forward;
                            stroke.push(start);
                        }
                        Ok(())
                    }
                }
            }
        }
    }

    pub fn add_point(&mut self, point: Point2f) -> Result<(), SketchError> {
        let stroke = self.stroke.as_mut().ok_or(SketchError::NoActiveStroke)?;

        match self.append_mode {
            StrokeAppendMode::Forward => stroke.push(point),
            StrokeAppendMode::Backward => stroke.insert_at_start(point),
        }

        Ok(())
    }

    pub fn end_stroke(&mut self) -> Result<Option<Stroke>, SketchError> {
        // Take the stroke out of self
        let mut stroke = self.stroke.take().ok_or(SketchError::NoActiveStroke)?;

        // check_closed mutably
        if !stroke.check_closed(self.close_epsilon) {
            // Not closed → put it back
            self.stroke = Some(stroke);
            return Ok(None);
        }

        // Closed → return it
        Ok(Some(stroke))
    }
}
