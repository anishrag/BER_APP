//! The Phase-3 (Objects) entry UI, shown while a room pin is OPEN:
//!
//! - a big CATEGORY BAR across the top of the canvas (icon buttons, just below
//!   the phase signpost) — clicking a category opens its options;
//! - a floating OPTIONS window — the open room's objects so far + Done, and (once
//!   a category is picked) its subtype / count / note / photos / Add.
//!
//! UI-only: reads the controller's `ActiveObjectRoomView` and emits
//! `SketchEvent`s; the controller owns the data + photo store.

use egui::{Align2, Color32, Id, RichText};

use super::{SketchEvent, SketchView};
use crate::controller::app_controller::AssessmentPhase;
use crate::ui::pages::events::UiEvent;
use crate::ui::widgets::{photo_button, PhotoConfig, PhotoQuality};
use ber_model::ber::objects::{ObjectCategory, ObjectKind};

/// A simple glyph per category for the big buttons. Emoji placeholders until a
/// proper icon set is wired — some may render as a box depending on the font.
fn category_icon(cat: ObjectCategory) -> &'static str {
    match cat {
        ObjectCategory::Vent => "🌬",
        ObjectCategory::Fan => "🌀",
        ObjectCategory::Light => "💡",
        ObjectCategory::Radiator => "♨",
        ObjectCategory::Boiler => "🔥",
        ObjectCategory::WaterCylinder => "🛢",
        ObjectCategory::Thermostat => "🌡",
        ObjectCategory::Chimney => "🏭",
        ObjectCategory::AtticHatch => "🪜",
        ObjectCategory::WeatherCompensation => "🌦",
        ObjectCategory::WaterFixture => "🚿",
        ObjectCategory::Other => "➕",
    }
}

pub(super) fn render<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    rect: egui::Rect,
    view: &mut SketchView<'_>,
    on_event: &mut F,
) {
    if !matches!(view.current_phase, AssessmentPhase::Objects) {
        return;
    }
    // No room open: a small hint to tap the plan.
    let Some(room) = view.active_object_room.as_ref() else {
        egui::Area::new(Id::new("ber_objects_hint"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.center_top() + egui::vec2(0.0, 52.0))
            .pivot(Align2::CENTER_TOP)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label("Tap the plan where you are to record objects here.");
                });
            });
        return;
    };
    let room_id = room.id;
    let title = if room.name.trim().is_empty() {
        "Objects here".to_string()
    } else {
        room.name.clone()
    };
    let present = room.categories_present.clone();
    let existing: Vec<(ber_model::ber::common::ItemId, String, u32, usize)> = room
        .objects
        .iter()
        .map(|o| (o.id, o.label.clone(), o.count, o.photo_count))
        .collect();

    // ── Big category bar across the top, below the phase signpost ──
    egui::Area::new(Id::new("ber_objects_category_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.center_top() + egui::vec2(0.0, 50.0))
        .pivot(Align2::CENTER_TOP)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width((rect.width() - 24.0).max(120.0));
                ui.horizontal_wrapped(|ui| {
                    for cat in ObjectCategory::ALL {
                        let here = present.contains(&cat);
                        let selected = *view.object_selected_category == Some(cat);
                        let icon = category_icon(cat);
                        let mark = if here { " ✓" } else { "" };
                        let btn = egui::Button::new(
                            RichText::new(format!("{icon}\n{}{mark}", cat.label())).size(12.0),
                        )
                        .min_size(egui::vec2(78.0, 50.0))
                        .selected(selected);
                        if ui.add(btn).clicked() {
                            // Toggle: clicking the open category closes its options.
                            if selected {
                                *view.object_selected_category = None;
                                *view.object_selected_kind = None;
                            } else {
                                *view.object_selected_category = Some(cat);
                                *view.object_selected_kind = cat.singleton();
                                view.object_other_name.clear();
                            }
                        }
                    }
                });
            });
        });

    // ── Floating options window: room context + the picked category's entry ──
    egui::Window::new("Objects in this room")
        .id(Id::new("ber_objects_panel"))
        .order(egui::Order::Foreground)
        .default_pos(rect.left_top() + egui::vec2(16.0, 150.0))
        .pivot(Align2::LEFT_TOP)
        .constrain_to(rect)
        .collapsible(true)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_min_width(260.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&title).strong().size(15.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Done").clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::CloseObjectRoom));
                    }
                });
            });

            if existing.is_empty() {
                ui.label(
                    RichText::new("No objects yet — pick a category above.")
                        .italics()
                        .color(Color32::from_gray(150)),
                );
            } else {
                for (oid, label, count, photos) in &existing {
                    ui.horizontal(|ui| {
                        ui.label(format!("{label}  ×{count}"));
                        if *photos > 0 {
                            ui.label(
                                RichText::new(format!("📷{photos}")).color(Color32::from_gray(150)),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✕").on_hover_text("Remove").clicked() {
                                on_event(UiEvent::Sketch(SketchEvent::DeleteObject {
                                    room: room_id,
                                    object: *oid,
                                }));
                            }
                        });
                    });
                }
            }

            // The picked category's options.
            if let Some(cat) = *view.object_selected_category {
                ui.separator();
                ui.label(RichText::new(cat.label()).strong());
                let subtypes = cat.subtypes();
                if !subtypes.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for kind in &subtypes {
                            let selected = view.object_selected_kind.as_ref() == Some(kind);
                            if ui
                                .selectable_label(selected, kind.subtype_label().to_string())
                                .clicked()
                            {
                                *view.object_selected_kind = Some(kind.clone());
                            }
                        }
                    });
                }
                if cat == ObjectCategory::Other {
                    ui.horizontal(|ui| {
                        ui.label("name");
                        ui.text_edit_singleline(view.object_other_name);
                    });
                    *view.object_selected_kind =
                        Some(ObjectKind::Other(view.object_other_name.clone()));
                }

                let kind_ready = view
                    .object_selected_kind
                    .as_ref()
                    .map(|k| !matches!(k, ObjectKind::Other(s) if s.trim().is_empty()))
                    .unwrap_or(false);
                if kind_ready {
                    ui.horizontal(|ui| {
                        ui.label("count");
                        ui.add(
                            egui::TextEdit::singleline(view.object_count_input)
                                .desired_width(48.0)
                                .hint_text("1"),
                        );
                        ui.label("note");
                        ui.add(
                            egui::TextEdit::singleline(view.object_note_input)
                                .desired_width(110.0)
                                .hint_text("optional"),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        for p in view.object_pending_photos.iter() {
                            let src = p.to_string_lossy();
                            ui.add(
                                egui::Image::new(format!("file://{src}"))
                                    .fit_to_exact_size(egui::vec2(40.0, 40.0))
                                    .maintain_aspect_ratio(true)
                                    .corner_radius(3.0),
                            );
                        }
                        if let Some(path) = photo_button(
                            ui,
                            "📷 Add photo",
                            true,
                            PhotoConfig {
                                default_quality: PhotoQuality::High,
                            },
                        ) {
                            view.object_pending_photos.push(path);
                        }
                    });
                    if ui
                        .add(egui::Button::new(RichText::new("Add").strong()))
                        .clicked()
                    {
                        if let Some(kind) = view.object_selected_kind.clone() {
                            let count = view.object_count_input.trim().parse::<u32>().unwrap_or(1);
                            let note = {
                                let n = view.object_note_input.trim();
                                (!n.is_empty()).then(|| n.to_string())
                            };
                            on_event(UiEvent::Sketch(SketchEvent::AddObject {
                                kind,
                                count,
                                note,
                                photos: view.object_pending_photos.clone(),
                                photo_quality: PhotoQuality::High,
                            }));
                            *view.object_selected_category = None;
                            *view.object_selected_kind = None;
                            view.object_count_input.clear();
                            view.object_note_input.clear();
                            view.object_other_name.clear();
                            view.object_pending_photos.clear();
                        }
                    }
                }
            }
        });
}
