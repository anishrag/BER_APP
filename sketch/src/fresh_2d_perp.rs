pub use crate::engine::{SketchEngine, SketchError, SketchModeBehaviour, StrokeAppendMode};
pub use crate::model::Stroke;

pub use domain::points::Point2f32;
pub use domain::sketch::EditTools;
#[derive(Debug, Default)]
pub struct Storey2DPerpWalls;

impl SketchModeBehaviour for Storey2DPerpWalls {
    fn start_stroke(
        &mut self,
        stroke: &mut Option<Stroke>,
        edit_tool: EditTools,
        close_epsilon: f32,
        snap_epsilon: f32,
        append_mode: &mut StrokeAppendMode,
        start: Point2f32,
    ) -> Result<(), SketchError> {
        match stroke {
            None => {
                *stroke = Some(Stroke::new(edit_tool, start));
                *append_mode = StrokeAppendMode::Forward;
                Ok(())
            }
            Some(stroke) => {
                if stroke.check_closed(close_epsilon) {
                    return Err(SketchError::StrokeAlreadyClosed);
                }

                let first = stroke.first_point();
                let last = stroke.last_point();

                let dist_to_start = (first - start).norm();
                let dist_to_end = (last - start).norm();

                let can_snap_start = dist_to_start <= snap_epsilon;
                let can_snap_end = dist_to_end <= snap_epsilon;

                match (can_snap_start, can_snap_end) {
                    (false, false) => Err(SketchError::SnapTooFar),
                    (true, false) => {
                        *append_mode = StrokeAppendMode::Backward;
                        stroke.insert_at_start(start);
                        Ok(())
                    }
                    (false, true) => {
                        *append_mode = StrokeAppendMode::Forward;
                        stroke.push(start);
                        Ok(())
                    }
                    (true, true) => {
                        if dist_to_start < dist_to_end {
                            *append_mode = StrokeAppendMode::Backward;
                            stroke.insert_at_start(start);
                        } else {
                            *append_mode = StrokeAppendMode::Forward;
                            stroke.push(start);
                        }
                        Ok(())
                    }
                }
            }
        }
    }

    fn add_point(
        &mut self,
        stroke: &mut Option<Stroke>,
        append_mode: StrokeAppendMode,
        point: Point2f32,
    ) -> Result<(), SketchError> {
        let stroke = stroke.as_mut().ok_or(SketchError::NoActiveStroke)?;

        match append_mode {
            StrokeAppendMode::Forward => stroke.push(point),
            StrokeAppendMode::Backward => stroke.insert_at_start(point),
        }

        Ok(())
    }

    fn end_stroke(
        &mut self,
        stroke: &mut Option<Stroke>,
        close_epsilon: f32,
    ) -> Result<Option<Stroke>, SketchError> {
        let mut local_stroke = stroke.take().ok_or(SketchError::NoActiveStroke)?;

        if !local_stroke.check_closed(close_epsilon) {
            *stroke = Some(local_stroke);
            return Ok(None);
        }

        Ok(Some(local_stroke))
    }
}
