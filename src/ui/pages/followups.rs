//! The "Follow ups" tab: the post-survey document/detail requests to send the
//! client. Checkboxes (auto-ticked from the survey, freely overridable) drive an
//! editable email draft; a one-shot "Send email" posts it to the client and
//! moves the job to `details_requested`. The assessor can also add per-BER
//! custom requests via the "+". All logic lives in the controller + `ber_model`;
//! this page is a thin renderer over [`FollowUpView`].

use egui::{Color32, RichText, Ui};

use ber_model::ber::common::ItemId;

use crate::controller::app_controller::{FollowUpRenameKey, FollowUpRowKey, FollowUpView};
use crate::ui::widgets::{text_input, text_input_multiline};

/// Intents raised by the Follow-ups tab.
#[derive(Debug)]
pub enum FollowUpEvent {
    /// Tick/untick a request row — built-in or custom (rebuilds the draft).
    Toggle(FollowUpRowKey),
    /// Rename a named thing shown on a sub-row (window/door type, construction
    /// group) — writes the durable model name the whole app uses, then
    /// regenerates the draft so the letter uses the new name.
    Rename {
        key: FollowUpRenameKey,
        name: String,
    },
    /// The assessor edited the email body.
    SetBody(String),
    /// Add a per-BER custom request point (label + email paragraph).
    AddCustom { label: String, content: String },
    /// Remove a custom request point.
    RemoveCustom(ItemId),
    /// Send the request email (one-shot).
    Send,
}

pub struct FollowUpsPage;

impl FollowUpsPage {
    /// Render the tab. `body` is the editable draft buffer (kept in sync with the
    /// view by the caller); `new_label`/`new_content` back the "add a request"
    /// fields; `rename` is the one in-flight inline rename (which sub-row +
    /// its typed buffer). The page emits events for every change.
    pub fn render<F>(
        ui: &mut Ui,
        view: &FollowUpView,
        body: &mut String,
        new_label: &mut String,
        new_content: &mut String,
        rename: &mut Option<(FollowUpRenameKey, String)>,
        mut on_event: F,
    ) where
        F: FnMut(FollowUpEvent),
    {
        ui.heading("Follow ups");
        ui.add_space(6.0);

        if view.sent {
            ui.label(
                RichText::new("✓ Request sent — the client has been emailed for these details.")
                    .strong()
                    .color(Color32::from_rgb(0x2e, 0x7d, 0x32)),
            );
            ui.label(RichText::new("This is a one-off send; the request is now locked.").weak());
            ui.add_space(12.0);
            ui.label(RichText::new("What was requested").strong());
            for p in view.points.iter().filter(|p| p.checked) {
                ui.label(format!("• {}", p.label));
            }
            ui.add_space(12.0);
            ui.label(RichText::new("Email sent").strong());
            ui.add_space(4.0);
            // Read-only view of the sent email.
            let mut sent = view.body.clone();
            ui.add_enabled(
                false,
                egui::TextEdit::multiline(&mut sent)
                    .desired_width(f32::INFINITY)
                    .desired_rows(16),
            );
            return;
        }

        ui.label(
            RichText::new(
                "Tick the details/documents to request from the client. Ticks are pre-set from \
                 the survey — adjust as needed. The draft below updates when you change a tick; \
                 edit it freely, then send.",
            )
            .weak(),
        );
        ui.add_space(10.0);

        ui.label(RichText::new("Requests").strong());
        ui.add_space(2.0);
        for row in &view.points {
            ui.horizontal(|ui| {
                let mut checked = row.checked;
                if ui.checkbox(&mut checked, &row.label).changed() {
                    on_event(FollowUpEvent::Toggle(row.key.clone()));
                }
                if row.removable {
                    if let FollowUpRowKey::Custom(id) = &row.key {
                        if ui
                            .small_button("✕")
                            .on_hover_text("Remove this custom request")
                            .clicked()
                        {
                            on_event(FollowUpEvent::RemoveCustom(id.clone()));
                        }
                    }
                }
            });
            // Per-thing sub-checks (extensions / surfaces / window & door
            // types), indented under their parent row.
            for sub in &row.sub_rows {
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    let mut checked = sub.checked;
                    if ui.checkbox(&mut checked, &sub.label).changed() {
                        on_event(FollowUpEvent::Toggle(sub.key.clone()));
                    }
                    // Inline rename for rows that show a durable model name —
                    // the letter uses THEIR name, so let the assessor clarify
                    // it here (same write as the Structure review's rename).
                    if let Some(rk) = &sub.rename_key {
                        let renaming_this = rename.as_ref().is_some_and(|(k, _)| k == rk);
                        if !renaming_this
                            && ui
                                .small_button("✏ Rename")
                                .on_hover_text(
                                    "Rename it for the client letter — this renames it \
                                     everywhere in the app",
                                )
                                .clicked()
                        {
                            *rename = Some((rk.clone(), sub.rename_prefill.clone()));
                        }
                    }
                });
                if let Some(rk) = &sub.rename_key {
                    let mut done = false;
                    if let Some((k, buf)) = rename.as_mut() {
                        if k == rk {
                            ui.horizontal(|ui| {
                                ui.add_space(48.0);
                                // `text_input` (never a raw TextEdit) so the
                                // native soft keyboard binds on Android —
                                // winit drops text events.
                                text_input(ui, buf, 220.0, "new name");
                                if ui.button("Save").clicked() {
                                    on_event(FollowUpEvent::Rename {
                                        key: k.clone(),
                                        name: buf.clone(),
                                    });
                                    done = true;
                                }
                                if ui.button("Cancel").clicked() {
                                    done = true;
                                }
                            });
                        }
                    }
                    if done {
                        *rename = None;
                    }
                    if let Some(hint) = &sub.rename_error {
                        ui.horizontal(|ui| {
                            ui.add_space(48.0);
                            ui.label(
                                RichText::new(format!("⚠ {hint}"))
                                    .color(Color32::from_rgb(0xc6, 0x28, 0x28)),
                            );
                        });
                    }
                }
            }
        }

        // Add a per-BER custom request.
        ui.add_space(6.0);
        ui.group(|ui| {
            ui.label(RichText::new("Add a BER-specific request").strong());
            ui.horizontal(|ui| {
                ui.label("Label:");
                // text_input / text_input_multiline (not raw TextEdits) so
                // the native soft keyboard binds on Android.
                text_input(ui, new_label, 240.0, "e.g. Septic tank cert");
            });
            ui.label(RichText::new("What to ask the client:").weak());
            text_input_multiline(ui, new_content, 3, "The paragraph added to the email…");
            let can_add = !new_label.trim().is_empty() || !new_content.trim().is_empty();
            if ui
                .add_enabled(can_add, egui::Button::new("+ Add request"))
                .clicked()
            {
                on_event(FollowUpEvent::AddCustom {
                    label: new_label.clone(),
                    content: new_content.clone(),
                });
                new_label.clear();
                new_content.clear();
            }
        });

        ui.add_space(14.0);
        ui.label(RichText::new("Email to the client").strong());
        ui.label(
            RichText::new(
                "Fill any [bracketed] blanks (supplier names, models, links) before sending.",
            )
            .weak(),
        );
        ui.add_space(4.0);
        // The editable draft: a multiline `text_input` so the native soft
        // keyboard binds on Android (with a real Enter for new lines). Every
        // edit streams into the controller — the SENT text is this buffer.
        if text_input_multiline(ui, body, 18, "").changed {
            on_event(FollowUpEvent::SetBody(body.clone()));
        }

        ui.add_space(12.0);
        if let Some(err) = &view.error {
            ui.label(RichText::new(err).color(Color32::from_rgb(0xc6, 0x28, 0x28)));
            ui.add_space(6.0);
        }

        ui.horizontal(|ui| {
            let btn = egui::Button::new(RichText::new("Send email").strong());
            if ui.add_enabled(!view.sending, btn).clicked() {
                on_event(FollowUpEvent::Send);
            }
            if view.sending {
                ui.add(egui::Spinner::new());
                ui.label(RichText::new("Sending…").weak());
            }
        });
        ui.label(
            RichText::new(
                "Sending emails the client and marks the job as details-requested. It can only \
                 be done once.",
            )
            .weak(),
        );
    }
}
