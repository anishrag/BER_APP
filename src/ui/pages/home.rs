use crate::engine::ber_repository::BerSummary;
use crate::ui::pages::events::UiEvent;
use ber_model::ber::common::ItemId;
use eframe::egui;

/// Home-page intents. The app shell handles these (controller calls +
/// navigation); the render fn only emits them.
#[derive(Debug)]
pub enum HomeEvent {
    /// Create a fresh record immediately and enter the workspace at Key Details.
    NewBer,
    /// Show/hide the inline saved-BER list (the shell refreshes the rows on open).
    ToggleLoadPanel,
    /// Open this saved BER and enter the workspace at Key Details.
    OpenBer(ItemId),
    /// Delete this saved BER (the render fn has already taken the two-step
    /// confirm; this fires on the confirming tap).
    DeleteBer(ItemId),
    /// Deep-copy this saved BER into a brand-new persisted record. Non-destructive,
    /// so no confirm — the shell duplicates and refreshes the list.
    DuplicateBer(ItemId),
}

/// Home-page UI state, owned by `BerApp`. `rows` is a cached snapshot of the
/// saved-BER list — refreshed by the shell when the panel opens and after a
/// delete, not re-queried per frame.
#[derive(Default)]
pub struct HomeState {
    pub show_load_panel: bool,
    pub rows: Vec<BerSummary>,
    /// Two-step delete: the row whose 🗑 was tapped once and is now showing
    /// its inline "Confirm delete" button. Cleared on cancel/confirm/toggle.
    pub pending_delete: Option<ItemId>,
}

pub struct HomePage;

impl HomePage {
    pub fn render<F>(ui: &mut egui::Ui, state: &mut HomeState, mut on_event: F)
    where
        F: FnMut(UiEvent),
    {
        ui.heading("BER App");
        ui.add_space(20.0);

        ui.horizontal(|ui| {
            if ui.button("New BER").clicked() {
                on_event(UiEvent::Home(HomeEvent::NewBer));
            }
            if ui
                .selectable_label(state.show_load_panel, "Load BER")
                .clicked()
            {
                on_event(UiEvent::Home(HomeEvent::ToggleLoadPanel));
            }
        });

        if state.show_load_panel {
            ui.add_space(10.0);
            ui.separator();
            ui.label(egui::RichText::new("Saved BERs").strong());
            ui.add_space(4.0);

            if state.rows.is_empty() {
                ui.label("No saved BERs yet.");
            }

            // Disjoint field borrows: iterate `rows` while mutating
            // `pending_delete` (the two-step confirm cursor).
            let HomeState {
                rows,
                pending_delete,
                ..
            } = state;
            for row in rows.iter() {
                ui.horizontal(|ui| {
                    let label =
                        format!("{}  —  {}", row.name, row.date_assessed.format("%-d %b %Y"));
                    if ui.button(label).clicked() {
                        on_event(UiEvent::Home(HomeEvent::OpenBer(row.id)));
                    }
                    // Duplicate is non-destructive — a plain button, no confirm.
                    if ui.button("⧉").clicked() {
                        on_event(UiEvent::Home(HomeEvent::DuplicateBer(row.id)));
                    }
                    if *pending_delete == Some(row.id) {
                        if ui
                            .button(
                                egui::RichText::new("Confirm delete")
                                    .color(egui::Color32::from_rgb(220, 80, 80)),
                            )
                            .clicked()
                        {
                            on_event(UiEvent::Home(HomeEvent::DeleteBer(row.id)));
                        }
                        if ui.button("Cancel").clicked() {
                            *pending_delete = None;
                        }
                    } else if ui.button("🗑").clicked() {
                        *pending_delete = Some(row.id);
                    }
                });
            }
        }

        ui.add_space(20.0);
        // Room below for future options (settings, export queue, …).
    }
}
