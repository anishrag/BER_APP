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
    // Deferred-photos stage: raised INSTEAD of the completion popup when
    // "Mark BER complete" was pressed while deferred sides still lack their
    // photo (defer = postpone to the end, never skip — see
    // `ExternalSide::photo_deferrable`). Capture buttons emit the same
    // `AddExternalPhoto` the Mapping panel uses (the controller clears each
    // deferral on capture); once none are pending, Continue re-confirms and
    // the BER actually completes. "Go back" leaves the BER un-complete.
    if view.show_deferred_photos_prompt {
        egui::Window::new("Deferred photos")
            .id(Id::new("deferred_photos_prompt"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                // Deferred sides still lacking a photo. `deferred_photo_sides`
                // never contains Front (not deferrable) and capture clears the
                // flag, so the photo cross-check is belt-and-braces.
                let pending: Vec<_> = view
                    .external_photos
                    .iter()
                    .filter(|(s, p)| view.deferred_photo_sides.contains(s) && p.is_none())
                    .map(|(s, _)| *s)
                    .collect();
                if pending.is_empty() {
                    ui.label(RichText::new("All deferred photos captured ✔").strong());
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(RichText::new("Continue — complete BER").strong())
                            .clicked()
                        {
                            on_event(UiEvent::Sketch(SketchEvent::ConfirmFinalReview));
                        }
                        if ui.button("Go back").clicked() {
                            on_event(UiEvent::Sketch(SketchEvent::DismissDeferredPhotosPrompt));
                        }
                    });
                    return;
                }
                ui.label(
                    RichText::new("You deferred these photos — take them now")
                        .strong()
                        .size(16.0),
                );
                ui.label("The BER can only complete once every deferred side has its photo.");
                ui.add_space(8.0);
                for side in pending {
                    ui.horizontal(|ui| {
                        ui.label(format!("⏳ {}", side.label()));
                        // The shared capture widget (desktop file dialog /
                        // Android camera), as on the Mapping panel — hi tier
                        // High for legible elevation detail.
                        if let Some((path, quality)) = crate::ui::widgets::photo_button_pair(
                            ui,
                            "Add",
                            true,
                            crate::ui::widgets::PhotoQuality::High,
                            false,
                        ) {
                            on_event(UiEvent::Sketch(SketchEvent::AddExternalPhoto {
                                side,
                                path,
                                quality,
                            }));
                        }
                    });
                }
                ui.add_space(10.0);
                if ui.button("Go back").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::DismissDeferredPhotosPrompt));
                }
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
            if view.site_satellite_image.is_none() {
                if ui.button("Load test image (practice)").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::LoadTestSatellite));
                }
                ui.label(
                    RichText::new("No satellite image yet — load a test one to try aligning.")
                        .size(11.0)
                        .color(egui::Color32::from_gray(150)),
                );
            }
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

/// The finish list a wall field offers: separating and external walls (the
/// heat-loss faces) additionally offer the insulated-plasterboard dry-lining;
/// internal walls keep the base list.
fn finish_options(field: WallFinishField) -> &'static [WallFinish] {
    match field {
        WallFinishField::Internal => &WallFinish::OPTIONS,
        WallFinishField::Separating | WallFinishField::External => {
            &WallFinish::OPTIONS_WITH_INSULATED
        }
    }
}

fn wall_combo<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    salt: &str,
    field: WallFinishField,
    current: WallFinish,
    on_event: &mut F,
) {
    let options = finish_options(field);
    egui::ComboBox::from_id_salt(("ber_final_wall", salt))
        .selected_text(current.label())
        .show_ui(ui, |ui| {
            for &o in options {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The Final-form wall pickers: separating and external walls offer
    /// "Insulated plasterboard"; internal walls keep the base finishes.
    #[test]
    fn separating_and_external_offer_insulated_plasterboard() {
        for field in [WallFinishField::Separating, WallFinishField::External] {
            assert!(
                finish_options(field).contains(&WallFinish::InsulatedPlasterboard),
                "{field:?} must offer the dry-lining finish"
            );
        }
        assert!(
            !finish_options(WallFinishField::Internal).contains(&WallFinish::InsulatedPlasterboard)
        );
        // No picker ever offers the `Unset` placeholder.
        for field in [
            WallFinishField::Internal,
            WallFinishField::Separating,
            WallFinishField::External,
        ] {
            assert!(!finish_options(field).contains(&WallFinish::Unset));
        }
    }
}
