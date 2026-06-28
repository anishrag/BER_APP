//! Floating overlays painted ON TOP of the sketch canvas (Foreground `Area`s,
//! the same layering pattern as the canvas view buttons + snap tags): the
//! three-phase progress signpost (top-centre), clickable to switch phase, with
//! the Stage A completion checklist folded in behind a triangle to its right
//! (collapsed by default, expanding to a live read-out under the bar).

use egui::{Align2, Color32, CornerRadius, Margin, Rect, RichText, Sense, Stroke};

use crate::controller::app_controller::{AssessmentPhase, ChecklistItem, ChecklistStatus};
use ber_model::ber::structural::SurfaceCounts;
use domain::sketch::DisplayMode;

/// Floating phase signpost centred at the top of the canvas: three boxes
/// (`Mapping → Structure → Objects`) joined by arrows, the `current` one
/// highlighted. The boxes are **clickable** to switch phase — returns the phase
/// the assessor clicked, or `None`. Switching onto Structure is gated on
/// `mapping_complete` (Structure also earns a green ✓ then); going back to
/// Mapping is always allowed; Objects isn't built yet, so it's never clickable.
/// Returns `(phase the assessor clicked, review button clicked)`. The checklist
/// for the current phase is folded into this bar: a triangle to the RIGHT of the
/// phase boxes expands/collapses it (collapsed by default; the expand state is
/// kept in egui memory). When expanded it drops down under the bar with the same
/// status rows + legend + Mapping "Review" button it had as a floating window.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn phase_bar(
    ui: &egui::Ui,
    rect: Rect,
    current: AssessmentPhase,
    mapping_reviewed: bool,
    structure_reviewed: bool,
    objects_reviewed: bool,
    details_reviewed: bool,
    items: &[ChecklistItem],
    show_review: bool,
    review_label: &str,
) -> (Option<AssessmentPhase>, bool, Rect) {
    let phases = [
        (AssessmentPhase::Mapping, "Mapping"),
        (AssessmentPhase::Structure, "Structure"),
        (AssessmentPhase::Objects, "Objects"),
        (AssessmentPhase::Final, "Final"),
    ];
    let mut clicked = None;
    let mut review_clicked = false;
    let expand_id = ui.id().with("phase_checklist_expanded");
    let mut expanded = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(expand_id))
        .unwrap_or(false);
    let bar = egui::Area::new(ui.id().with("canvas_phase_bar"))
        .order(egui::Order::Foreground)
        // `pivot(CENTER_TOP)` places the bar's top-centre at the canvas's
        // top-centre, so it stays centred over the canvas (not the whole
        // window — the landscape side panel offsets the two).
        .fixed_pos(rect.center_top() + egui::vec2(0.0, 8.0))
        .pivot(Align2::CENTER_TOP)
        .show(ui.ctx(), |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    for (i, (phase, label)) in phases.iter().enumerate() {
                        if i > 0 {
                            ui.label(RichText::new("→").size(18.0).color(Color32::from_gray(170)));
                        }
                        let active = *phase == current;
                        // Mapping always reachable (go back); Structure once
                        // Mapping is reviewed; Objects once Structure is reviewed.
                        let enabled = match phase {
                            AssessmentPhase::Mapping => true,
                            AssessmentPhase::Structure => mapping_reviewed,
                            AssessmentPhase::Objects => structure_reviewed,
                            AssessmentPhase::Final => objects_reviewed,
                        };
                        // Each phase earns its ✓ once its review is confirmed —
                        // each unlocks the next; Final is the dwelling's last.
                        let complete = match phase {
                            AssessmentPhase::Mapping => mapping_reviewed,
                            AssessmentPhase::Structure => structure_reviewed,
                            AssessmentPhase::Objects => objects_reviewed,
                            AssessmentPhase::Final => details_reviewed,
                        };
                        // The CURRENT phase's box carries the checklist-toggle
                        // triangle (to the right of its label); the others don't.
                        let (switch_clicked, triangle_clicked) = phase_box(
                            ui,
                            label,
                            active,
                            complete,
                            enabled,
                            active.then_some(expanded),
                        );
                        if switch_clicked && enabled {
                            clicked = Some(*phase);
                        }
                        if triangle_clicked {
                            expanded = !expanded;
                        }
                    }
                });
                if expanded {
                    ui.add_space(4.0);
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(260.0);
                        review_clicked = checklist_body(ui, items, show_review, review_label);
                    });
                }
            });
        });
    ui.ctx().data_mut(|d| d.insert_temp(expand_id, expanded));
    (clicked, review_clicked, bar.response.rect)
}

/// One phase box — filled + outlined, highlighted when it's the active phase,
/// prefixed with a green ✓ when complete, dimmed when not yet reachable.
///
/// `checklist_expanded` is `Some(is_expanded)` ONLY for the current phase: its
/// box then carries a checklist-toggle triangle to the right of the label.
/// Returns `(switch_clicked, triangle_clicked)`. The whole box is the phase-SWITCH
/// target only when it's NOT the active phase; on the active phase the triangle
/// owns the clicks instead, so tapping it toggles the checklist without also
/// "clicking the button" (and the active phase never switches anyway).
fn phase_box(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    complete: bool,
    enabled: bool,
    checklist_expanded: Option<bool>,
) -> (bool, bool) {
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
    let mut triangle_clicked = false;
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
                // Current phase: the checklist-toggle triangle, right of the label.
                if let Some(exp) = checklist_expanded {
                    ui.add_space(3.0);
                    let tri = if exp { "▼" } else { "▶" };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(tri).color(text_col).size(13.0))
                                .frame(false),
                        )
                        .on_hover_text("Show / hide checklist")
                        .clicked()
                    {
                        triangle_clicked = true;
                    }
                }
            });
        });
    // The box is a phase-switch target only when NOT active (the active phase's
    // triangle owns the clicks). No outer interact on the active box, so the
    // triangle tap stays clean.
    let switch_clicked = if active {
        false
    } else {
        let resp = ui.interact(
            inner.response.rect,
            ui.id().with(("phase_box", label)),
            Sense::click(),
        );
        if enabled {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
        } else {
            resp.clicked()
        }
    };
    (switch_clicked, triangle_clicked)
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

/// The per-phase checklist body — status rows (a red/yellow/green dot recomputed
/// each frame from `items`) + the legend + the Mapping "Review" button. Rendered
/// inline under the phase bar when its triangle is expanded. Returns true if the
/// review button was clicked.
fn checklist_body(
    ui: &mut egui::Ui,
    items: &[ChecklistItem],
    show_review: bool,
    review_label: &str,
) -> bool {
    if items.is_empty() {
        ui.label(RichText::new("Open a BER to track progress.").weak());
        return false;
    }
    let mut review_clicked = false;
    for item in items {
        ui.horizontal(|ui| {
            status_dot(ui, item.status);
            ui.label(item.label);
        });
    }
    ui.add_space(2.0);
    ui.separator();
    legend(ui);
    // The per-phase action button (Review / Mark BER complete) — once every item
    // is green. `reviewed` already-done variants show in the label.
    if show_review {
        ui.add_space(4.0);
        if ui.button(RichText::new(review_label).strong()).clicked() {
            review_clicked = true;
        }
    }
    review_clicked
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
