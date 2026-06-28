//! The Phase-4 ("Final") wizard: a floating window with three steps — the
//! details form (floor / wall finish / heating), the north alignment, and the
//! sheltered sides — then "BER complete". UI-only: reads the controller's
//! Phase-4 view-data and emits `SketchEvent`s.
//!
//! Steps 2 (north) and 3 (sheltered) are placeholders here; their canvas
//! interactions land in the next stages.

use egui::{Align2, Id, RichText};

use super::{SketchEvent, SketchView};
use crate::controller::app_controller::{AssessmentPhase, WallFinishField};
use crate::ui::pages::events::UiEvent;
use ber_model::ber::final_details::{FloorKind, HeatingKind, WallFinish};

pub(super) fn render<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    _rect: egui::Rect,
    view: &mut SketchView<'_>,
    on_event: &mut F,
) {
    if !matches!(view.current_phase, AssessmentPhase::Final) {
        return;
    }

    egui::Window::new("Final details")
        .id(Id::new("ber_final_panel"))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 90.0))
        .collapsible(true)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_min_width(320.0);

            // Step selector.
            ui.horizontal(|ui| {
                for (i, label) in [(0u8, "1 · Details"), (1, "2 · North"), (2, "3 · Sheltered")]
                {
                    if ui.selectable_label(*view.final_step == i, label).clicked() {
                        *view.final_step = i;
                    }
                }
            });
            ui.separator();

            match *view.final_step {
                0 => details_form(ui, view, on_event),
                1 => {
                    ui.label("North alignment — align the plan to the satellite image.");
                    ui.label(
                        RichText::new("(coming next)")
                            .italics()
                            .color(egui::Color32::from_gray(150)),
                    );
                }
                _ => {
                    ui.label("Sheltered sides — tap each side of the plan.");
                    ui.label(
                        RichText::new("(coming next)")
                            .italics()
                            .color(egui::Color32::from_gray(150)),
                    );
                }
            }

            ui.separator();
            if view.details_reviewed {
                ui.label(RichText::new("BER marked complete ✓").strong());
            } else if ui
                .button(RichText::new("✓ Mark BER complete").strong())
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::ConfirmFinalReview));
            }
        });
}

fn details_form<F: FnMut(UiEvent)>(ui: &mut egui::Ui, view: &SketchView<'_>, on_event: &mut F) {
    let d = view.final_details;

    egui::Grid::new("ber_final_form")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Floor");
            floor_combo(ui, d.floor, on_event);
            ui.end_row();

            ui.label("Internal walls");
            wall_combo(
                ui,
                "iw",
                WallFinishField::Internal,
                d.internal_wall,
                on_event,
            );
            ui.end_row();

            ui.label("Separating walls");
            wall_combo(
                ui,
                "sw",
                WallFinishField::Separating,
                d.separating_wall,
                on_event,
            );
            ui.end_row();

            ui.label("External walls");
            wall_combo(
                ui,
                "ew",
                WallFinishField::External,
                d.external_wall,
                on_event,
            );
            ui.end_row();

            ui.label("Heating");
            heating_combo(ui, d.heating, on_event);
            ui.end_row();
        });
}

fn floor_combo<F: FnMut(UiEvent)>(ui: &mut egui::Ui, current: FloorKind, on_event: &mut F) {
    egui::ComboBox::from_id_salt("ber_final_floor")
        .selected_text(current.label())
        .show_ui(ui, |ui| {
            for o in FloorKind::OPTIONS {
                if ui.selectable_label(current == o, o.label()).clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::SetFinalFloor(o)));
                }
            }
        });
}

fn wall_combo<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    salt: &str,
    field: WallFinishField,
    current: WallFinish,
    on_event: &mut F,
) {
    egui::ComboBox::from_id_salt(("ber_final_wall", salt))
        .selected_text(current.label())
        .show_ui(ui, |ui| {
            for o in WallFinish::OPTIONS {
                if ui.selectable_label(current == o, o.label()).clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::SetFinalWallFinish(field, o)));
                }
            }
        });
}

fn heating_combo<F: FnMut(UiEvent)>(ui: &mut egui::Ui, current: HeatingKind, on_event: &mut F) {
    egui::ComboBox::from_id_salt("ber_final_heating")
        .selected_text(current.label())
        .show_ui(ui, |ui| {
            for o in HeatingKind::OPTIONS {
                if ui.selectable_label(current == o, o.label()).clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::SetFinalHeating(o)));
                }
            }
        });
}
