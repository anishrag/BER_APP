// ui/adapters/sketch_egui.rs
use egui::epaint::{ColorMode, PathStroke};
use egui::{Color32, Pos2};

use sketch::model::{Point2f, Stroke};
use sketch::SketchEvent;

use domain::sketch::SketchMode;

use crate::ui::pages::sketch::EguiStroke;

/// Convert a domain Stroke into something egui can render
impl From<&Stroke> for EguiStroke {
    fn from(s: &Stroke) -> Self {
        Self {
            points: s.points.iter().map(|p| Pos2::new(p.x, p.y)).collect(),
            stroke: PathStroke {
                width: match s.mode {
                    //                    SketchMode::Eraser => 20.0,
                    _ => 2.0,
                },
                color: ColorMode::Solid(Color32::WHITE),
                ..Default::default()
            },
        }
    }
}

impl From<Stroke> for EguiStroke {
    fn from(s: Stroke) -> Self {
        Self::from(&s)
    }
}

pub fn pos2_to_point2f(pos: Pos2) -> Point2f {
    sketch::model::Point2f::new(pos.x, pos.y)
}

/// Helper to adapt a slice of domain strokes
pub fn adapt_strokes(strokes: &Stroke) -> EguiStroke {
    strokes.into()
}

/// Optional: helper to send UI events to the sketch engine
/// UI pages can call this closure instead of talking to controller directly
pub fn handle_ui_event<F>(event: SketchEvent, mut on_event: F)
where
    F: FnMut(SketchEvent),
{
    on_event(event);
}
