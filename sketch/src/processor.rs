// ===== sketch/src/processor.rs =====
use crate::model::Stroke;

#[derive(Debug)]
pub struct FloorPlan {
    pub area: f32,
    pub perimeter: f32,
}

pub struct FloorPlanProcessor;

impl FloorPlanProcessor {
    pub fn process(stroke: &Stroke) -> Option<FloorPlan> {
        if !stroke.is_closed || stroke.points.len() < 4 {
            return None;
        }

        let mut area = 0.0;
        let mut perimeter = 0.0;

        for w in stroke.points.windows(2) {
            let p1 = w[0];
            let p2 = w[1];
            area += (p1.x * p2.y) - (p2.x * p1.y);

            let dx = p2.x - p1.x;
            let dy = p2.y - p1.y;
            perimeter += (dx * dx + dy * dy).sqrt();
        }

        Some(FloorPlan {
            area: area.abs() * 0.5,
            perimeter,
        })
    }
}
