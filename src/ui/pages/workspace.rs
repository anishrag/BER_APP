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
                (Section::FollowUps, "Follow ups"),
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

    egui::CentralPanel::default().show(ctx, |ui| match section {
        // The sketch canvas fills the panel and owns its pointer input — no
        // scroll wrapper (drag-to-scroll would fight stroke capture, and its
        // floating windows already dodge the keyboard via their constrain
        // rects).
        Section::Sketch => body(ui),
        // Form sections scroll. While the native keyboard is up, shrink the
        // scroll viewport by its inset so the shared fields' one-shot
        // scroll-into-view (`pads::scroll_clear_of_keyboard`) can bring a
        // covered field above the keyboard; when the keyboard hides the inset
        // returns to 0 and the viewport regains full height. Never below 40%
        // of the panel so an oversized keyboard can't squash the form into
        // nothing — the pages reserve the residue themselves
        // (`keyboard_scroll_reserve` at their content end). On desktop the
        // inset is always 0: full-height area, no scrollbar when the content
        // fits.
        Section::KeyDetails | Section::FollowUps => {
            let inset = crate::ui::widgets::ime_inset_bottom_points(ctx);
            let avail = ui.available_height();
            let max_h = (avail - inset).max(avail * 0.4);
            egui::ScrollArea::vertical()
                // Distinct scroll state per section, so a scrolled-down Key
                // Details doesn't leave More Details half-scrolled.
                .id_salt(match section {
                    Section::KeyDetails => "workspace_scroll_key_details",
                    _ => "workspace_scroll_more_details",
                })
                .auto_shrink([false, false])
                .max_height(max_h)
                .show(ui, |ui| body(ui));
        }
    });
}
