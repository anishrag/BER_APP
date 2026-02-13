// ===== sketch/src/geometry.rs =====
use crate::model::Stroke;
use domain::points::Point2f32;
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: Point2f32,
    pub max: Point2f32,
}

impl Stroke {
    pub fn add_point(&mut self, p: Point2f32) {
        self.points.push(p);
    }

    pub fn check_closed(&mut self, epsilon: f32) -> bool {
        if self.points.len() < 3 {
            return false;
        }

        let first = self.points.first().copied().unwrap();
        let last = self.points.last().copied().unwrap();

        let dx = first.x - last.x;
        let dy = first.y - last.y;
        let dist2 = dx * dx + dy * dy;

        if dist2 <= epsilon * epsilon {
            self.is_closed = true;
            self.points.pop();
            self.points.push(first);
            true
        } else {
            false
        }
    }

    pub fn bounding_box(&self) -> Option<Aabb> {
        let mut iter = self.points.iter();
        let first = iter.next()?;

        let mut min = *first;
        let mut max = *first;

        for p in iter {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }

        Some(Aabb { min, max })
    }
}
