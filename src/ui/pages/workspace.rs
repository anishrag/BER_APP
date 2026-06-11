//! The BER-workspace chrome: a top bar (BER name • section tabs • Exit BER),
//! a bottom Continue bar, and the active section's content between them.
use crate::ui::pages::events::{Page, Section, UiEvent};
use eframe::egui;

/// Render the workspace chrome around `body` (the active section's content).
///
/// Top bar: the BER name (read-only here — edited in Key Details), the three
/// section tabs (always clickable, no gating), and a right-aligned "Exit BER".
/// Bottom bar: "Continue →" advancing Key Details → Sketch → More Details
/// (absent on More Details — that slot is reserved for Finish/Export later).
///
/// `on_event` receives only chrome intents (`NavigateTo` / `ExitBer`); section
/// content dispatches its own events inside `body`. The two closures are
/// separate so the shell can collect chrome intents into locals while `body`
/// borrows the app state.
pub fn workspace_chrome<F, B>(
    ctx: &egui::Context,
    section: Section,
    ber_name: &str,
    mut on_event: F,
    body: B,
) where
    F: FnMut(UiEvent),
    B: FnOnce(&mut egui::Ui),
{
    egui::TopBottomPanel::top("workspace_top").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(ber_name).strong());
            ui.separator();
            for (s, label) in [
                (Section::KeyDetails, "Key Details"),
                (Section::Sketch, "Sketch"),
                (Section::MoreDetails, "More Details"),
            ] {
                if ui.selectable_label(section == s, label).clicked() && section != s {
                    on_event(UiEvent::NavigateTo(Page::Workspace(s)));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Exit BER").clicked() {
                    on_event(UiEvent::ExitBer);
                }
            });
        });
    });

    if let Some(next) = section.next() {
        egui::TopBottomPanel::bottom("workspace_bottom").show(ctx, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Continue →").clicked() {
                    on_event(UiEvent::NavigateTo(Page::Workspace(next)));
                }
            });
        });
    }

    egui::CentralPanel::default().show(ctx, |ui| body(ui));
}
