use egui::epaint::{PathShape, PathStroke};
use egui::Pos2;
use strum::IntoEnumIterator;

use crate::ui::pages::events::UiEvent;

use domain::sketch::{DisplayMode, EditTools, SketchMode};

/// The UI-friendly version of a stroke
pub struct EguiStroke {
    pub points: Vec<Pos2>,
    pub stroke: PathStroke,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SketchEvent {
    StartStroke(Pos2),
    AddPoint(Pos2),
    EndStroke(Pos2),
    ChangeMode(SketchMode),
    ChangeDisplay(DisplayMode),
    ChangeEditTool(EditTools),
    StrokeClosed { stroke_idx: usize },
}

#[derive(Default)]
pub struct SketchPage;
impl SketchPage {
    pub fn render<F>(
        ui: &mut egui::Ui,
        current: Option<EguiStroke>,
        mode: SketchMode,
        display: DisplayMode,
        edit_tool: EditTools,
        mut on_event: F,
    ) where
        F: FnMut(UiEvent),
    {
        // Allocate vertical layout manually
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.vertical(|ui| {
                // --- Top toolbar: SketchMode ---
                if let Some(new_mode) = Self::mode_toolbar(ui, mode) {
                    on_event(UiEvent::Sketch(SketchEvent::ChangeMode(new_mode)));
                }

                // --- Middle toolbar: DisplayMode ---
                if let Some(new_display) = Self::display_toolbar(ui, display) {
                    on_event(UiEvent::Sketch(SketchEvent::ChangeDisplay(new_display)));
                }

                ui.separator();

                let bottom_toolbar_height = 30.0; // adjust if your toolbar is taller
                let separator_height = ui.spacing().item_spacing.y;
                let canvas_height =
                    ui.available_height() - bottom_toolbar_height - separator_height;

                let (rect, response) = ui.allocate_exact_size(
                    egui::Vec2::new(ui.available_width(), canvas_height),
                    egui::Sense::drag(),
                );

                let painter = ui.painter_at(rect);

                // Draw canvas background
                painter.rect_filled(rect, 0.0, egui::Color32::from_gray(30));

                // Canvas input events
                if response.drag_started() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let local_pos = pos - rect.left_top().to_vec2();
                        on_event(UiEvent::Sketch(SketchEvent::StartStroke(local_pos)));
                    }
                }
                if response.dragged() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let local_pos = pos - rect.left_top().to_vec2();
                        on_event(UiEvent::Sketch(SketchEvent::AddPoint(local_pos)));
                    }
                }
                if response.drag_stopped() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let local_pos = pos - rect.left_top().to_vec2();
                        on_event(UiEvent::Sketch(SketchEvent::EndStroke(local_pos)));
                    }
                }

                if let Some(stroke) = current {
                    draw_stroke(&painter, rect, stroke);
                }

                ui.separator();

                // --- Bottom toolbar: EditTools ---
                if let Some(new_tool) = Self::edit_toolbar(ui, edit_tool) {
                    on_event(UiEvent::Sketch(SketchEvent::ChangeEditTool(new_tool)));
                }
            });
        });
    }

    fn mode_toolbar(ui: &mut egui::Ui, mode: SketchMode) -> Option<SketchMode> {
        let mut change_mode: Option<SketchMode> = None;
        ui.horizontal(|ui| {
            for mode_item in SketchMode::iter() {
                let selected = mode == mode_item;
                if ui
                    .selectable_label(selected, format!("{:?}", mode_item))
                    .clicked()
                {
                    change_mode = Some(mode_item);
                }
            }
        });
        change_mode
    }

    fn display_toolbar(ui: &mut egui::Ui, display: DisplayMode) -> Option<DisplayMode> {
        let mut change_display: Option<DisplayMode> = None;
        ui.horizontal(|ui| {
            for display_item in DisplayMode::iter() {
                let selected = display == display_item;
                if ui
                    .selectable_label(selected, format!("{:?}", display_item))
                    .clicked()
                {
                    change_display = Some(display_item);
                }
            }
        });
        change_display
    }

    fn edit_toolbar(ui: &mut egui::Ui, tool: EditTools) -> Option<EditTools> {
        let mut change_tool: Option<EditTools> = None;
        ui.horizontal(|ui| {
            for tool_item in EditTools::iter() {
                let selected = tool == tool_item;
                if ui
                    .selectable_label(selected, format!("{:?}", tool_item))
                    .clicked()
                {
                    change_tool = Some(tool_item);
                }
            }
        });
        change_tool
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
