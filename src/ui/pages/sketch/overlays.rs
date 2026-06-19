//! Floating overlays painted ON TOP of the sketch canvas (Foreground `Area`s /
//! a `Window`, the same layering pattern as the canvas view buttons + snap
//! tags): the three-phase progress signpost (top-centre) and the Stage A
//! completion checklist (a draggable, collapsible window). Both read view data
//! and emit no events — the phase boxes are a roadmap, not a navigator (only
//! Stage A exists today), and the checklist is a live read-out.

use egui::{Align2, Color32, CornerRadius, Margin, Rect, RichText, Sense, Stroke};

use crate::controller::app_controller::{AssessmentPhase, ChecklistItem, ChecklistStatus};
use datatypes::ber::structural::SurfaceCounts;
use domain::sketch::DisplayMode;

/// Floating phase signpost centred at the top of the canvas: three boxes
/// (`Mapping → Structure → Objects`) joined by arrows, the `current` one
/// highlighted. The boxes are **clickable** to switch phase — returns the phase
/// the assessor clicked, or `None`. Switching onto Structure is gated on
/// `mapping_complete` (Structure also earns a green ✓ then); going back to
/// Mapping is always allowed; Objects isn't built yet, so it's never clickable.
#[must_use]
pub fn phase_bar(
    ui: &egui::Ui,
    rect: Rect,
    current: AssessmentPhase,
    mapping_complete: bool,
) -> Option<AssessmentPhase> {
    let phases = [
        (AssessmentPhase::Mapping, "Mapping"),
        (AssessmentPhase::Structure, "Structure"),
        (AssessmentPhase::Objects, "Objects"),
    ];
    let mut clicked = None;
    egui::Area::new(ui.id().with("canvas_phase_bar"))
        .order(egui::Order::Foreground)
        // `pivot(CENTER_TOP)` places the bar's top-centre at the canvas's
        // top-centre, so it stays centred over the canvas (not the whole
        // window — the landscape side panel offsets the two).
        .fixed_pos(rect.center_top() + egui::vec2(0.0, 8.0))
        .pivot(Align2::CENTER_TOP)
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                for (i, (phase, label)) in phases.iter().enumerate() {
                    if i > 0 {
                        ui.label(RichText::new("→").size(18.0).color(Color32::from_gray(170)));
                    }
                    let active = *phase == current;
                    // Mapping always reachable (go back); Structure once Mapping
                    // is complete; Objects not built yet.
                    let enabled = match phase {
                        AssessmentPhase::Mapping => true,
                        AssessmentPhase::Structure => mapping_complete,
                        AssessmentPhase::Objects => false,
                    };
                    // Only Mapping has a completion criterion today; Structure
                    // earns its ✓ in a later step (per-surface completion).
                    let complete = matches!(phase, AssessmentPhase::Mapping) && mapping_complete;
                    if phase_box(ui, label, active, complete, enabled).clicked()
                        && !active
                        && enabled
                    {
                        clicked = Some(*phase);
                    }
                }
            });
        });
    clicked
}

/// One phase box — filled + outlined, highlighted when it's the active phase,
/// prefixed with a green ✓ when complete, dimmed when not yet reachable. The
/// whole box is a click target (returns its `Response`) so the signpost can
/// switch phases; a reachable, non-active box shows a pointer cursor.
fn phase_box(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    complete: bool,
    enabled: bool,
) -> egui::Response {
    let (fill, stroke_col, text_col) = if active {
        (
            Color32::from_rgb(40, 92, 160),
            Color32::from_rgb(120, 170, 235),
            Color32::WHITE,
        )
    } else if enabled {
        (
            Color32::from_gray(48),
            Color32::from_gray(82),
            Color32::from_gray(190),
        )
    } else {
        // Not reachable yet — dimmer still.
        (
            Color32::from_gray(38),
            Color32::from_gray(60),
            Color32::from_gray(110),
        )
    };
    let inner = egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(if active { 2.0 } else { 1.0 }, stroke_col))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if complete {
                    // Same green as the checklist's "all storeys done" dot.
                    ui.label(
                        RichText::new("✓")
                            .color(Color32::from_rgb(80, 180, 95))
                            .size(15.0)
                            .strong(),
                    );
                }
                let mut text = RichText::new(label).color(text_col).size(15.0);
                if active {
                    text = text.strong();
                }
                ui.label(text);
            });
        });
    // Make the whole framed box (not just the text) the click target.
    let resp = ui.interact(
        inner.response.rect,
        ui.id().with(("phase_box", label)),
        Sense::click(),
    );
    if enabled && !active {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// A small centred banner just under the phase signpost naming the surface
/// kind currently highlighted (+ its count), so the shaded faces on the canvas
/// are explicit ("Highlighting Roofs · 3"). Painted only when a single surface
/// is selected (`Walls` / `Roofs` / `Floors`); nothing otherwise.
pub fn highlight_caption(ui: &egui::Ui, rect: Rect, display: DisplayMode, counts: SurfaceCounts) {
    let (label, count, accent) = match display {
        DisplayMode::Walls => ("Walls", counts.walls, Color32::from_rgb(230, 230, 230)),
        DisplayMode::Roofs => ("Roofs", counts.roofs, Color32::from_rgb(220, 160, 80)),
        DisplayMode::Floors => ("Floors", counts.floors, Color32::from_rgb(120, 170, 240)),
        _ => return,
    };
    egui::Area::new(ui.id().with("canvas_highlight_caption"))
        .order(egui::Order::Foreground)
        // Just under the phase signpost (which sits at center_top + 8).
        .fixed_pos(rect.center_top() + egui::vec2(0.0, 48.0))
        .pivot(Align2::CENTER_TOP)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(Color32::from_gray(28))
                .stroke(Stroke::new(1.0, accent))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Highlighting")
                                .size(12.0)
                                .color(Color32::from_gray(170)),
                        );
                        ui.label(RichText::new(label).size(13.0).strong().color(accent));
                        ui.label(
                            RichText::new(format!("· {count}"))
                                .size(13.0)
                                .color(Color32::from_gray(200)),
                        );
                    });
                });
        });
}

/// Floating, draggable, collapsible per-phase checklist over the canvas. Each
/// item shows a red / yellow / green status dot recomputed every frame from
/// `items` (so it tracks edits live). The window title reflects the current
/// `phase`; it keeps its own position + collapsed state in egui memory (keyed
/// by that title), constrained to the canvas `rect`.
pub fn checklist_window(
    ui: &egui::Ui,
    rect: Rect,
    phase: AssessmentPhase,
    items: &[ChecklistItem],
) {
    let title = match phase {
        AssessmentPhase::Mapping => "Mapping checklist",
        AssessmentPhase::Structure => "Structure checklist",
        AssessmentPhase::Objects => "Objects checklist",
    };
    egui::Window::new(title)
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
