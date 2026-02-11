use egui::epaint::{PathShape, PathStroke};
use egui::Pos2;
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

/// The UI-friendly version of a stroke
pub struct EguiStroke {
    pub points: Vec<Pos2>,
    pub stroke: PathStroke,
}

#[derive(Default, Clone, Copy, PartialEq, EnumIter, Debug)]
pub enum SketchMode {
    #[default]
    DrawPerpWalls,
    SelectRect,
    AmendWallsNonPerp,
    AmendWallsCircular,
    Door,
    Window,
    Eraser,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SketchEvent {
    StartStroke(Pos2),
    AddPoint(Pos2),
    EndStroke,
    ChangeMode(SketchMode),
    StrokeClosed { stroke_idx: usize },
}

#[derive(Default)]
pub struct SketchPage;

impl SketchPage {
    pub fn render<F>(
        ui: &mut egui::Ui,
        current: Option<EguiStroke>,
        mode: SketchMode,
        mut on_event: F,
    ) where
        F: FnMut(SketchEvent),
    {
        if let Some(new_mode) = Self::mode_toolbar(ui, mode) {
            on_event(SketchEvent::ChangeMode(new_mode));
        }
        ui.separator();
        Self::canvas(ui, current, on_event);
    }

    fn canvas<F>(ui: &mut egui::Ui, current: Option<EguiStroke>, mut on_event: F)
    where
        F: FnMut(SketchEvent),
    {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
        let painter = ui.painter_at(rect);

        // Background
        painter.rect_filled(rect, 0.0, egui::Color32::from_gray(30));

        // -------- INPUT --------
        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                on_event(SketchEvent::StartStroke(pos));
            }
        }

        if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                on_event(SketchEvent::AddPoint(pos));
            }
        }

        if response.drag_stopped() {
            on_event(SketchEvent::EndStroke);
        }

        if let Some(stroke) = current {
            draw_stroke(&painter, rect, stroke);
        }
    }

    fn mode_toolbar(ui: &mut egui::Ui, mode: SketchMode) -> Option<SketchMode> {
        let mut change_mode: Option<SketchMode> = None;
        ui.horizontal(|ui| {
            for mode_item in SketchMode::iter() {
                let selected = mode == mode_item;
                if ui
                    .selectable_label(selected, format!("{:?}", mode))
                    .clicked()
                {
                    change_mode = Some(mode_item);
                }
            }
        });
        change_mode
    }
}

fn draw_stroke(painter: &egui::Painter, rect: egui::Rect, stroke: EguiStroke) {
    if stroke.points.len() < 2 {
        return;
    }

    let points: Vec<egui::Pos2> = stroke
        .points
        .iter()
        .map(|p| egui::pos2(rect.left() + p.x, rect.top() + p.y))
        .collect();

    painter.add(PathShape::line(points, stroke.stroke.clone()));
}
