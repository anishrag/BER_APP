//! Phase-4 ("Final") UI pieces:
//! - [`render`] draws the completion modals ("BER complete" popup + the dummy
//!   summary), independent of the current phase (set on confirm).
//! - [`details_panel`] is the details side of the Final-phase takeover (the
//!   dropdown form + the Align / Sheltered mode buttons + the active mode's
//!   controls) — the canvas lays it out beside the plan and calls it.
//!
//! UI-only: reads the controller's Phase-4 view-data and emits `SketchEvent`s.

use egui::{Align2, Id, RichText};

use super::{SketchEvent, SketchView};
use crate::controller::app_controller::{FinalMode, WallFinishField};
use crate::ui::pages::events::UiEvent;
use ber_model::ber::final_details::{FloorKind, HeatingKind, WallFinish};

/// The completion popup + summary modals (shown over everything when active).
pub(super) fn render<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    _rect: egui::Rect,
    view: &mut SketchView<'_>,
    on_event: &mut F,
) {
    if view.show_summary {
        egui::Window::new("BER summary")
            .id(Id::new("ber_summary"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                ui.label(RichText::new("BER summary").strong().size(18.0));
                ui.add_space(6.0);
                ui.label(
                    RichText::new("(summary — coming soon)")
                        .italics()
                        .color(egui::Color32::from_gray(150)),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Back to phases").clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::CloseSummary));
                    }
                    if ui
                        .button(RichText::new("Save & exit BER").strong())
                        .clicked()
                    {
                        on_event(UiEvent::ExitBer);
                    }
                });
            });
        return;
    }
    if view.show_ber_complete {
        egui::Window::new("BER complete")
            .id(Id::new("ber_complete_popup"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(RichText::new("BER is complete 🎉").strong().size(18.0));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("Take me to summary").strong())
                        .clicked()
                    {
                        on_event(UiEvent::Sketch(SketchEvent::OpenSummary));
                    }
                    if ui.button("Go back").clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::DismissBerComplete));
                    }
                });
            });
    }
}

/// The details side of the Final-phase takeover: the dropdown form + mode
/// controls. Sheltered-side taps + image gestures happen on the plan (canvas).
pub(super) fn details_panel<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    view: &SketchView<'_>,
    on_event: &mut F,
) {
    ui.label(RichText::new("Final details").strong().size(16.0));
    ui.add_space(6.0);

    match view.final_mode {
        FinalMode::Align => {
            ui.label("Move / rotate / zoom the satellite image to line it up");
            ui.label("with the plan, then OK.");
            ui.label(
                RichText::new(
                    "Two fingers (tablet) · left-drag move, right-drag rotate, scroll zoom (PC)",
                )
                .size(11.0)
                .color(egui::Color32::from_gray(150)),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("OK — aligned").strong()).clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::ConfirmAlign));
                }
                if ui.button("Recenter").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::ResetNorthImage));
                }
            });
        }
        FinalMode::Sheltered => {
            ui.label("Tap each side of the plan to set how sheltered it is.");
            ui.add_space(4.0);
            for &side in &ber_model::ber::common::ExternalSide::ALL {
                ui.label(format!(
                    "{}:  {}",
                    side.label(),
                    view.sheltering.get(side).label()
                ));
            }
            ui.add_space(6.0);
            if ui.button(RichText::new("OK — done").strong()).clicked() {
                on_event(UiEvent::Sketch(SketchEvent::SetFinalMode(FinalMode::Form)));
            }
        }
        FinalMode::Form => {
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
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let img = view.site_satellite_image.is_some();
                if ui
                    .add_enabled(img, egui::Button::new("Align image to north"))
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::SetFinalMode(FinalMode::Align)));
                }
                if ui.button("Sheltered sides").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::SetFinalMode(
                        FinalMode::Sheltered,
                    )));
                }
            });
            ui.label(
                RichText::new("Tick \"Mark BER complete\" in the checklist when done.")
                    .size(11.0)
                    .color(egui::Color32::from_gray(150)),
            );
        }
    }
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
