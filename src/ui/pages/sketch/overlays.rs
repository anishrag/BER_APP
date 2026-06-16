//! Floating overlays painted ON TOP of the sketch canvas (Foreground `Area`s /
//! a `Window`, the same layering pattern as the canvas view buttons + snap
//! tags): the three-phase progress signpost (top-centre) and the Stage A
//! completion checklist (a draggable, collapsible window). Both read view data
//! and emit no events — the phase boxes are a roadmap, not a navigator (only
//! Stage A exists today), and the checklist is a live read-out.

use egui::{Align2, Color32, CornerRadius, Margin, Rect, RichText, Stroke};

use crate::controller::app_controller::{ChecklistItem, ChecklistStatus};

/// The assessor's three-phase job. Only `Mapping` (Stage A — the drawing tools
/// on the left) is built today; the later two are shown greyed as a roadmap.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Mapping,
    Stage2,
    Stage3,
}

/// Floating phase signpost centred at the top of the canvas: three boxes
/// (`Mapping → Stage 2 → Stage 3`) joined by arrows, the `current` one
/// highlighted. Non-interactive — a progress signpost, not a tab strip.
pub fn phase_bar(ui: &egui::Ui, rect: Rect, current: Phase) {
    let phases = [
        (Phase::Mapping, "Mapping"),
        (Phase::Stage2, "Stage 2"),
        (Phase::Stage3, "Stage 3"),
    ];
    egui::Area::new(ui.id().with("canvas_phase_bar"))
        .order(egui::Order::Foreground)
        // `pivot(CENTER_TOP)` places the bar's top-centre at the canvas's
        // top-centre, so it stays centred over the canvas (not the whole
        // window — the landscape side panel offsets the two).
        .fixed_pos(rect.center_top() + egui::vec2(0.0, 8.0))
        .pivot(Align2::CENTER_TOP)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                for (i, (phase, label)) in phases.iter().enumerate() {
                    if i > 0 {
                        ui.label(RichText::new("→").size(18.0).color(Color32::from_gray(170)));
                    }
                    phase_box(ui, label, *phase == current);
                }
            });
        });
}

/// One phase box — filled + outlined, highlighted when it's the active phase.
fn phase_box(ui: &mut egui::Ui, label: &str, active: bool) {
    let (fill, stroke_col, text_col) = if active {
        (
            Color32::from_rgb(40, 92, 160),
            Color32::from_rgb(120, 170, 235),
            Color32::WHITE,
        )
    } else {
        (
            Color32::from_gray(48),
            Color32::from_gray(82),
            Color32::from_gray(150),
        )
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(if active { 2.0 } else { 1.0 }, stroke_col))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            let mut text = RichText::new(label).color(text_col).size(15.0);
            if active {
                text = text.strong();
            }
            ui.label(text);
        });
}

/// Floating, draggable, collapsible Stage A checklist over the canvas. Each
/// item shows a red / yellow / green status dot recomputed every frame from
/// `items` (so it tracks edits live). The window keeps its own position +
/// collapsed state in egui memory (keyed by its title), constrained to the
/// canvas `rect`.
pub fn checklist_window(ui: &egui::Ui, rect: Rect, items: &[ChecklistItem]) {
    egui::Window::new("Stage A checklist")
        .order(egui::Order::Foreground)
        .default_pos(rect.left_top() + egui::vec2(16.0, 56.0))
        .constrain_to(rect)
        .collapsible(true)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            if items.is_empty() {
                ui.label(RichText::new("Open a BER to track progress.").weak());
                return;
            }
            for item in items {
                ui.horizontal(|ui| {
                    status_dot(ui, item.status);
                    ui.label(item.label);
                });
            }
            ui.add_space(2.0);
            ui.separator();
            legend(ui);
        });
}

/// A filled circle in the item's status colour.
fn status_dot(ui: &mut egui::Ui, status: ChecklistStatus) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 5.0, status_color(status));
}

/// Red = incomplete on this storey, yellow = done here but not everywhere,
/// green = done on every storey.
fn status_color(status: ChecklistStatus) -> Color32 {
    match status {
        ChecklistStatus::Incomplete => Color32::from_rgb(205, 70, 60),
        ChecklistStatus::ThisStorey => Color32::from_rgb(222, 180, 60),
        ChecklistStatus::AllStoreys => Color32::from_rgb(80, 180, 95),
    }
}

/// One-line legend spelling out what the three dot colours mean.
fn legend(ui: &mut egui::Ui) {
    let small = |ui: &mut egui::Ui, status: ChecklistStatus, text: &str| {
        ui.horizontal(|ui| {
            status_dot(ui, status);
            ui.label(RichText::new(text).size(11.0).weak());
        });
    };
    small(ui, ChecklistStatus::Incomplete, "this storey: to do");
    small(ui, ChecklistStatus::ThisStorey, "this storey done");
    small(ui, ChecklistStatus::AllStoreys, "all storeys done");
}
