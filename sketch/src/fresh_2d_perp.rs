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
        // Decide whether to extend an in-flight stroke or start a fresh one.
        // Fresh start when:
        //   - no in-flight stroke
        //   - the in-flight stroke is already closed (shouldn't happen under
        //     normal flow since end_stroke clears closed strokes, but defensive)
        //   - the new touch is too far from both endpoints to snap
        let extend_with = stroke.as_mut().and_then(|s| {
            if s.check_closed(close_epsilon) {
                return None;
            }
            let first = s.first_point();
            let last = s.last_point();
            let dist_to_start = (first - start).norm();
            let dist_to_end = (last - start).norm();
            let can_snap_start = dist_to_start <= snap_epsilon;
            let can_snap_end = dist_to_end <= snap_epsilon;

            match (can_snap_start, can_snap_end) {
                (false, false) => None,
                (true, false) => Some(StrokeAppendMode::Backward),
                (false, true) => Some(StrokeAppendMode::Forward),
                (true, true) => Some(if dist_to_start < dist_to_end {
                    StrokeAppendMode::Backward
                } else {
                    StrokeAppendMode::Forward
                }),
            }
        });

        match extend_with {
            Some(mode) => {
                let s = stroke
                    .as_mut()
                    .expect("extend_with is Some ⇒ stroke is Some");
                match mode {
                    StrokeAppendMode::Forward => s.push(start),
                    StrokeAppendMode::Backward => s.insert_at_start(start),
                }
                *append_mode = mode;
            }
            None => {
                // Touch is far from both endpoints (or there was no stroke).
                // Discard any in-flight stroke and start a new one here.
                *stroke = Some(Stroke::new(edit_tool, start));
                *append_mode = StrokeAppendMode::Forward;
            }
        }

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{DEFAULT_CLOSE_EPSILON as CLOSE_EPS, DEFAULT_SNAP_EPSILON as SNAP_EPS};

    fn stroke_with(points: &[(f32, f32)]) -> Stroke {
        let mut s = Stroke::new(
            EditTools::DrawPerpWalls,
            Point2f32::new(points[0].0, points[0].1),
        );
        for &(x, y) in &points[1..] {
            s.push(Point2f32::new(x, y));
        }
        s
    }

    #[test]
    fn start_stroke_with_no_existing_creates_fresh() {
        let mut behaviour = Storey2DPerpWalls;
        let mut stroke: Option<Stroke> = None;
        let mut mode = StrokeAppendMode::Forward;

        behaviour
            .start_stroke(
                &mut stroke,
                EditTools::DrawPerpWalls,
                CLOSE_EPS,
                SNAP_EPS,
                &mut mode,
                Point2f32::new(50.0, 50.0),
            )
            .unwrap();

        let s = stroke.expect("stroke was created");
        assert_eq!(s.points.len(), 1);
        assert_eq!(s.points[0], Point2f32::new(50.0, 50.0));
    }

    #[test]
    fn start_stroke_near_end_extends_forward() {
        let mut behaviour = Storey2DPerpWalls;
        let mut stroke = Some(stroke_with(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]));
        let mut mode = StrokeAppendMode::Forward;

        // Touch within snap_epsilon of the last point (100, 100).
        behaviour
            .start_stroke(
                &mut stroke,
                EditTools::DrawPerpWalls,
                CLOSE_EPS,
                SNAP_EPS,
                &mut mode,
                Point2f32::new(110.0, 110.0),
            )
            .unwrap();

        let s = stroke.unwrap();
        assert_eq!(s.points.len(), 4);
        assert_eq!(*s.points.last().unwrap(), Point2f32::new(110.0, 110.0));
        assert!(matches!(mode, StrokeAppendMode::Forward));
    }

    #[test]
    fn start_stroke_near_start_extends_backward() {
        let mut behaviour = Storey2DPerpWalls;
        let mut stroke = Some(stroke_with(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]));
        let mut mode = StrokeAppendMode::Forward;

        // Touch within snap_epsilon of the first point (0, 0).
        behaviour
            .start_stroke(
                &mut stroke,
                EditTools::DrawPerpWalls,
                CLOSE_EPS,
                SNAP_EPS,
                &mut mode,
                Point2f32::new(-10.0, 5.0),
            )
            .unwrap();

        let s = stroke.unwrap();
        assert_eq!(s.points.len(), 4);
        assert_eq!(s.points[0], Point2f32::new(-10.0, 5.0));
        assert!(matches!(mode, StrokeAppendMode::Backward));
    }

    #[test]
    fn start_stroke_far_from_both_endpoints_discards_and_starts_fresh() {
        let mut behaviour = Storey2DPerpWalls;
        let mut stroke = Some(stroke_with(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]));
        let mut mode = StrokeAppendMode::Forward;

        // Touch is >100 from both (0,0) and (100,100).
        behaviour
            .start_stroke(
                &mut stroke,
                EditTools::DrawPerpWalls,
                CLOSE_EPS,
                SNAP_EPS,
                &mut mode,
                Point2f32::new(500.0, 500.0),
            )
            .unwrap();

        let s = stroke.expect("a fresh stroke was started");
        assert_eq!(s.points.len(), 1, "previous stroke was discarded");
        assert_eq!(s.points[0], Point2f32::new(500.0, 500.0));
        assert!(matches!(mode, StrokeAppendMode::Forward));
    }
}
