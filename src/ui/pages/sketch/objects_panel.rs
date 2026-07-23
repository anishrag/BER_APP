//! The Phase-3 (Objects) entry UI, shown while a room pin is OPEN:
//!
//! - a big CATEGORY BAR across the top of the canvas (icon buttons, just below
//!   the phase signpost) — clicking a category opens its options;
//! - a floating OPTIONS window — the open room's objects so far + Done, and (once
//!   a category is picked) its subtype / count / note / photos / Add.
//!
//! UI-only: reads the controller's `ActiveObjectRoomView` and emits
//! `SketchEvent`s; the controller owns the data + photo store.

use egui::{Align2, Id, RichText};

use super::{SketchEvent, SketchView};
use crate::controller::app_controller::AssessmentPhase;
use crate::ui::pages::events::UiEvent;
use crate::ui::widgets::{metre_input, photo_button_pair, text_input, HintExt, PhotoQuality};
use ber_model::ber::objects::{ObjectCategory, ObjectKind};

/// A simple glyph per category for the big buttons. Emoji placeholders until a
/// proper icon set is wired — some may render as a box depending on the font.
fn category_icon(cat: ObjectCategory) -> &'static str {
    match cat {
        ObjectCategory::Vent => "🌬",
        ObjectCategory::Fan => "🌀",
        ObjectCategory::Light => "💡",
        ObjectCategory::Radiator => "♨",
        ObjectCategory::HeatingSystem => "🔥",
        ObjectCategory::WaterCylinder => "🛢",
        ObjectCategory::Chimney => "🏭",
        ObjectCategory::AtticHatch => "🪜",
        ObjectCategory::WaterFixture => "🚿",
        ObjectCategory::SolarPanel => "☀",
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
    // The Objects REVIEW takeover owns the screen — don't draw the category bar /
    // details / hint over it.
    if view.object_review_view.is_some() {
        return;
    }
    // The ATTIC bucket button, top-right: the attic isn't a storey, so it has
    // no plan pin to tap — this opens (creating on first use) the house-level
    // attic room; the usual panel below then adds objects to it (e.g. a
    // heat-recovery unit).
    egui::Area::new(Id::new("ber_objects_attic_button"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.right_top() + egui::vec2(-12.0, 52.0))
        .pivot(Align2::RIGHT_TOP)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let open_on_attic = view.active_object_room.as_ref().is_some_and(|r| r.is_attic);
                let btn =
                    egui::Button::new(RichText::new("🪜 Attic").size(13.0)).selected(open_on_attic);
                if ui
                    .add(btn)
                    .hint("Record objects in the attic space")
                    .clicked()
                    && !open_on_attic
                {
                    on_event(UiEvent::Sketch(SketchEvent::OpenAtticRoom));
                }
            });
        });
    // No room open: a small hint to tap the plan.
    let Some(room) = view.active_object_room.as_ref() else {
        egui::Area::new(Id::new("ber_objects_hint"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.center_top() + egui::vec2(0.0, 52.0))
            .pivot(Align2::CENTER_TOP)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(
                        "Tap the plan — or a surface in the 3D view — to record objects there.",
                    );
                });
            });
        return;
    };
    let present = room.categories_present.clone();
    // The pending-photo buffers are scoped to THIS room (photos parked here
    // never resurface in another room's commit).
    let room_id = room.id;

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
                        let mark = if here { " ✔" } else { "" };
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
                            }
                            // A switch starts a fresh entry: reset the count /
                            // note / name buffers (cheap text — OK to drop).
                            // Pending PHOTOS are NOT dropped: they're buffered
                            // per kind, so the previous kind's photos stay
                            // parked and reappear if it's re-selected.
                            view.object_count_input.clear();
                            view.object_note_input.clear();
                            view.object_other_name.clear();
                            // Re-arm the auto-camera for the new selection.
                            *view.object_auto_photo_for = None;
                        }
                    }
                });
            });
        });

    // ── Details window: shown only once a category is picked from the top bar.
    // Positioned bottom-left, clear of the centred top bar and the pin dropdown.
    // OK adds the object to the room (appears in the pin dropdown) and closes this
    // window; the room stays selected so the next category can be added.
    if let Some(cat) = *view.object_selected_category {
        egui::Window::new(format!("Add {}", cat.label()))
            .id(Id::new("ber_objects_details"))
            .order(egui::Order::Foreground)
            .default_pos(rect.left_bottom() + egui::vec2(16.0, -240.0))
            .pivot(Align2::LEFT_TOP)
            .constrain_to(rect)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(240.0);
                // The pin sits on a SLOPED surface picked in 3D (a roof plane /
                // sloped ceiling): show its pitch read-only, so e.g. a solar
                // panel's roof angle is on screen while it's recorded. (No
                // object subtype carries an angle field yet — when one does,
                // prefill it from this value.)
                if let Some(pitch) = room.surface_pitch_deg {
                    ui.label(
                        RichText::new(format!("Roof pitch: {pitch:.0}°"))
                            .color(egui::Color32::from_rgb(220, 160, 80)),
                    );
                }
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
                    let prev_name = view.object_other_name.clone();
                    ui.horizontal(|ui| {
                        ui.label("name");
                        // Use the app's `text_input` (not raw `text_edit_singleline`)
                        // so the on-screen keyboard opens on the tablet — without it
                        // the name can't be typed, so the object never becomes
                        // `kind_ready` and the count / photo / OK controls (below)
                        // never appear.
                        text_input(ui, view.object_other_name, 140.0, "name");
                    });
                    // The photo buffer is keyed by the FULL `Other(name)`:
                    // follow a live edit of the name so a photo taken
                    // mid-typing stays with THIS entry (while a different
                    // parked name — e.g. "bath" vs "shower" — keeps its own).
                    if *view.object_other_name != prev_name {
                        view.object_pending_photos.rename_other(
                            room_id,
                            &prev_name,
                            view.object_other_name,
                        );
                    }
                    *view.object_selected_kind =
                        Some(ObjectKind::Other(view.object_other_name.clone()));
                }

                let kind_ready = view
                    .object_selected_kind
                    .as_ref()
                    .map(|k| !matches!(k, ObjectKind::Other(s) if s.trim().is_empty()))
                    .unwrap_or(false);
                if kind_ready {
                    let kind = view
                        .object_selected_kind
                        .clone()
                        .expect("kind_ready implies a selected kind");
                    // A photo is mandatory for this instance when the kind's
                    // policy says so — always, or (first-instance kinds) until
                    // one of that kind is recorded house-wide.
                    let already_recorded = view.object_recorded_kinds.contains(&kind);
                    let photo_required = kind.photo_required(already_recorded);
                    // Only the SELECTED kind's pending photos IN THIS ROOM
                    // count — another kind's (or room's) parked photos must
                    // not satisfy this kind's gates.
                    let have_photo = !view
                        .object_pending_photos
                        .for_kind(room_id, &kind)
                        .is_empty();

                    // Boilers + the central ventilation unit REQUIRE a
                    // make/model name (the office looks the unit up); the
                    // battery REQUIRES its capacity in kWh.
                    let name_required = kind.name_required();
                    let kwh_required = kind.kwh_required();
                    ui.horizontal(|ui| {
                        ui.label("count");
                        metre_input(ui, view.object_count_input, 48.0, "1");
                        ui.label(if name_required { "name" } else { "note" });
                        text_input(
                            ui,
                            view.object_note_input,
                            110.0,
                            if name_required { "make & model" } else { "optional" },
                        );
                        if kwh_required {
                            ui.label("kWh");
                            metre_input(ui, view.object_kwh_input, 48.0, "e.g. 5");
                        }
                    });
                    let have_name = !view.object_note_input.trim().is_empty();
                    let kwh_value = view.object_kwh_input.trim().parse::<f32>().ok();
                    if name_required && !have_name {
                        ui.label(
                            RichText::new("✎ A name (make & model) is required for this item")
                                .small()
                                .color(egui::Color32::from_rgb(220, 160, 80)),
                        );
                    }
                    if kwh_required && kwh_value.is_none() {
                        ui.label(
                            RichText::new("⚡ The battery capacity (kWh) is required")
                                .small()
                                .color(egui::Color32::from_rgb(220, 160, 80)),
                        );
                    }

                    // Auto-open the camera ONCE for a still-unsatisfied mandatory
                    // photo (latched per selected kind, so a cancel doesn't loop
                    // — the assessor taps 📷 to retry).
                    let auto_open = photo_required
                        && !have_photo
                        && view.object_auto_photo_for.as_ref() != Some(&kind);
                    if auto_open {
                        *view.object_auto_photo_for = Some(kind.clone());
                    }
                    ui.horizontal_wrapped(|ui| {
                        for (p, _quality) in
                            view.object_pending_photos.for_kind(room_id, &kind).iter()
                        {
                            let src = p.to_string_lossy();
                            ui.add(
                                egui::Image::new(format!("file://{src}"))
                                    .fit_to_exact_size(egui::vec2(40.0, 40.0))
                                    .maintain_aspect_ratio(true)
                                    .corner_radius(3.0),
                            );
                        }
                        // TEMPORARY dual hi/lo capture (see `photo_button_pair`).
                        if let Some(captured) = photo_button_pair(
                            ui,
                            "📷 Add photo",
                            true,
                            PhotoQuality::High,
                            auto_open,
                        ) {
                            view.object_pending_photos.push(room_id, &kind, captured);
                        }
                    });
                    if photo_required && !have_photo {
                        ui.label(
                            RichText::new("📷 A photo is required for this item")
                                .small()
                                .color(egui::Color32::from_rgb(220, 160, 80)),
                        );
                    }

                    // OK is gated on the photo requirement — a mandatory photo
                    // must be taken before the object can be added — plus the
                    // name / kWh requirements above.
                    let can_ok = (!photo_required || have_photo)
                        && (!name_required || have_name)
                        && (!kwh_required || kwh_value.is_some());
                    if ui
                        .add_enabled(can_ok, egui::Button::new(RichText::new("OK").strong()))
                        .clicked()
                    {
                        let count = view.object_count_input.trim().parse::<u32>().unwrap_or(1);
                        let note = {
                            let n = view.object_note_input.trim();
                            (!n.is_empty()).then(|| n.to_string())
                        };
                        // Commit ONLY this room+kind's pending photos (consumes
                        // the buffer); other kinds' / rooms' parked photos are
                        // untouched.
                        let photos = view.object_pending_photos.take(room_id, &kind);
                        on_event(UiEvent::Sketch(SketchEvent::AddObject {
                            kind,
                            count,
                            note,
                            kwh: if kwh_required { kwh_value } else { None },
                            photos,
                        }));
                        *view.object_selected_category = None;
                        *view.object_selected_kind = None;
                        *view.object_auto_photo_for = None;
                        view.object_count_input.clear();
                        view.object_note_input.clear();
                        view.object_kwh_input.clear();
                        view.object_other_name.clear();
                    }
                }
            });
    }
}
