//! The "Follow ups" tab: the post-survey document/detail requests to send the
//! client. Checkboxes (auto-ticked from the survey, freely overridable) drive an
//! editable email draft; a one-shot "Send email" posts it to the client and
//! moves the job to `details_requested`. All logic lives in the controller +
//! `ber_model`; this page is a thin renderer over [`FollowUpView`].

use egui::{Color32, RichText, Ui};

use ber_model::ber::followup::FollowUpPointId;

use crate::controller::app_controller::FollowUpView;

/// Intents raised by the Follow-ups tab.
#[derive(Debug)]
pub enum FollowUpEvent {
    /// Tick/untick a request point (rebuilds the draft).
    Toggle(FollowUpPointId),
    /// The assessor edited the email body.
    SetBody(String),
    /// Send the request email (one-shot).
    Send,
}

pub struct FollowUpsPage;

impl FollowUpsPage {
    /// Render the tab. `body` is the editable draft buffer (kept in sync with the
    /// view by the caller); the page edits it in place and emits `SetBody` on
    /// change.
    pub fn render<F>(ui: &mut Ui, view: &FollowUpView, body: &mut String, mut on_event: F)
    where
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
            let mut checked = row.checked;
            if ui.checkbox(&mut checked, row.label).changed() {
                on_event(FollowUpEvent::Toggle(row.id));
            }
        }

        ui.add_space(14.0);
        ui.label(RichText::new("Email to the client").strong());
        ui.label(
            RichText::new(
                "Fill any [bracketed] blanks (supplier names, models, links) before sending.",
            )
            .weak(),
        );
        ui.add_space(4.0);
        if ui
            .add(
                egui::TextEdit::multiline(body)
                    .desired_width(f32::INFINITY)
                    .desired_rows(18),
            )
            .changed()
        {
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
