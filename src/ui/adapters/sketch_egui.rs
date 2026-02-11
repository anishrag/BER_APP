// ui/adapters/sketch_egui.rs
use egui::epaint::{ColorMode, PathStroke};
use egui::{Color32, Pos2};

use sketch::model::{DrawMode, Stroke};
use sketch::SketchEvent;

use crate::ui::pages::sketch::{EguiStroke, SketchMode};

/// Convert a domain Stroke into something egui can render
impl From<&Stroke> for EguiStroke {
    fn from(s: &Stroke) -> Self {
        Self {
            points: s.points.iter().map(|p| Pos2::new(p.x, p.y)).collect(),
            stroke: PathStroke {
                width: match s.mode {
                    DrawMode::Eraser => 20.0,
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

impl From<DrawMode> for SketchMode {
    fn from(mode: DrawMode) -> Self {
        match mode {
            DrawMode::FloorPlan => SketchMode::DrawPerpWalls,
            DrawMode::Room => SketchMode::DrawPerpWalls,
            DrawMode::Wall => SketchMode::DrawPerpWalls,
            DrawMode::Door => SketchMode::Door,
            DrawMode::Window => SketchMode::Window,
            DrawMode::Eraser => SketchMode::Eraser,
        }
    }
}

impl From<SketchMode> for DrawMode {
    fn from(mode: SketchMode) -> Self {
        match mode {
            SketchMode::DrawPerpWalls => DrawMode::FloorPlan,
            SketchMode::SelectRect => DrawMode::FloorPlan, // No direct mapping, default to FloorPlan
            SketchMode::AmendWallsNonPerp => DrawMode::Wall,
            SketchMode::AmendWallsCircular => DrawMode::Wall,
            SketchMode::Door => DrawMode::Door,
            SketchMode::Window => DrawMode::Window,
            SketchMode::Eraser => DrawMode::Eraser,
        }
    }
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
