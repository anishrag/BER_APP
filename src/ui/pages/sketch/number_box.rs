//! The floating NUMBER-ENTRY box — the *rendering* half of the number entry
//! (the input engine, parsing and buffer edits, lives UI-free in
//! [`crate::number_entry`]).
//!
//! Every *single-value* measurement prompt of the Sketch page (wall dimension,
//! carved-area position, post-drag distance, floor-slab thickness, floor/
//! ceiling step amounts, roof apex/ridge/rect heights and lengths) is entered
//! here instead of in the bottom prompt dock. The box is a draggable floating
//! window over the canvas: it keeps its own position in egui memory, so the
//! assessor parks it clear of the drawing and it stays put.
//!
//! Layout, top to bottom: the question (the window's title bar — also the drag
//! handle), the running **expression** (e.g. `3.2 + 1.5`), a numeric keypad
//! with a big **+** that starts another term, and the prompt's action buttons
//! (Apply / Skip / Set / Raise / … — supplied per prompt). Each key is just a
//! [`Key`] applied to the buffer by [`crate::number_entry::apply`]; the value
//! is read back with [`parse_dimension_expression`]. So "type a number, press
//! +, type another, Accept" sums them — and a future Bluetooth source feeds the
//! same `Key`s without this file changing.
//!
//! Only ONE single-value prompt is ever active at a time — the controller gates
//! the amount stages behind `current_position_prompt.is_none()`, and the
//! dimension/position/edit-drag prompts form an exclusive chain — so a single
//! box reproduces the old dock behaviour exactly. Multi-FIELD prompts
//! (vertex-drop's up-to-four distances, an opening's width+height+sill, an
//! exposure section's start+end) still live in the dock; they aren't a single
//! value.

use egui::{Align2, Color32, Id, RichText};

use super::{SketchEvent, SketchView};
use crate::number_entry::{apply, parse_dimension_expression, Key};
use crate::ui::pages::events::UiEvent;
use crate::ui::widgets::HintExt;

/// Minimum finger-target size for keypad keys (mirrors the on-screen pads).
const KEY: f32 = 46.0;

/// One action button shown under the keypad.
struct Action {
    label: String,
    hint: &'static str,
    /// Disabled until the expression parses (Apply / Set / Raise / …). `false`
    /// for buttons that fire regardless of the buffer (Skip / Cancel / Centre /
    /// Cycle / Keep-same-height).
    needs_value: bool,
    /// Hardware-Enter in the expression field triggers this action. At most one
    /// action per prompt is the primary.
    primary: bool,
    /// Clear the expression buffer after firing (the prefilled prompts —
    /// position, roof lengths — keep their value instead).
    clear: bool,
    /// Builds the event from the parsed value (0.0 for `needs_value == false`).
    event: Box<dyn Fn(f32) -> SketchEvent>,
}

impl Action {
    fn new(
        label: impl Into<String>,
        hint: &'static str,
        event: impl Fn(f32) -> SketchEvent + 'static,
    ) -> Self {
        Action {
            label: label.into(),
            hint,
            needs_value: true,
            primary: false,
            clear: true,
            event: Box::new(event),
        }
    }
    fn primary(mut self) -> Self {
        self.primary = true;
        self
    }
    /// Fires regardless of the buffer, and never clears it.
    fn bare(mut self) -> Self {
        self.needs_value = false;
        self.clear = false;
        self
    }
    fn keep_buffer(mut self) -> Self {
        self.clear = false;
        self
    }
    /// Re-enable buffer clearing after `bare()` (Skip/Cancel arms that
    /// historically wiped the field even though they ignore its value).
    fn with_clear(mut self) -> Self {
        self.clear = true;
        self
    }
}

/// Render the floating number box for whichever single-value prompt is active.
/// Renders nothing (no window) when none is — its remembered position persists
/// in egui memory across appearances.
pub(super) fn render<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    rect: egui::Rect,
    view: &mut SketchView<'_>,
    on_event: &mut F,
) {
    use manip_2d_plan::ParameterAxis::{X, Y};

    // ── Thickness (new-storey flow's first stage) ───────────────────────────
    if view
        .new_storey_prompt
        .as_ref()
        .is_some_and(|p| p.thickness_pending)
    {
        let actions = vec![
            Action::new(
                "Set",
                "Use this slab thickness (it sets the storey's floor level)",
                |v| SketchEvent::AssignFloorThickness(v as f64),
            )
            .primary()
            .keep_buffer(),
            Action::new("Use 0.30", "Accept the typical 0.30 m slab", |_| {
                SketchEvent::AssignFloorThickness(0.30)
            })
            .bare(),
        ];
        show_box(
            ctx,
            rect,
            "Floor thickness to the storey below (m)",
            Some("Drawing is paused until the thickness is set."),
            "e.g. 0.30",
            view.new_storey_error,
            view.new_storey_thickness_input,
            &actions,
            on_event,
        );
        return;
    }

    // ── Wall dimension ──────────────────────────────────────────────────────
    if let Some(p) = view.dim_prompt.as_ref() {
        let title = if view.dim_scale_set {
            "Length of highlighted wall (m)".to_string()
        } else {
            "Length of highlighted wall (first — sets scale)".to_string()
        };
        let sub = format!("currently {:.2}", p.current_length);
        let actions = vec![
            Action::new("Apply", "Apply this dimension and move to the next", |v| {
                SketchEvent::ApplyDimension(v)
            })
            .primary(),
            Action::new(
                "Skip",
                "Skip this dimension, ask about a different wall",
                |_| SketchEvent::SkipDimension,
            )
            .bare()
            // Skip historically cleared the field too.
            .with_clear(),
        ];
        show_box(
            ctx,
            rect,
            &title,
            Some(&sub),
            "e.g. 4.2",
            view.dim_error,
            view.dim_input,
            &actions,
            on_event,
        );
        return;
    }

    // ── Carved-area position ────────────────────────────────────────────────
    if let Some(p) = view.position_prompt.as_ref() {
        let direction = match p.axis {
            X => "Horizontal",
            Y => "Vertical",
        };
        let title = format!("{direction} distance from the highlighted wall (m)");
        let mut actions = vec![
            Action::new("Apply", "Pin the area at exactly this distance", |v| {
                SketchEvent::ApplyPositionDimension(v)
            })
            .primary()
            .keep_buffer(),
            Action::new("Skip", "Accept the drawn position as-is", |_| {
                SketchEvent::SkipPositionDimension
            })
            .bare(),
        ];
        if p.offers_centre {
            actions.push(
                Action::new("Centre", "Centre it within its ceiling area", |_| {
                    SketchEvent::CentreRoofPosition
                })
                .bare(),
            );
        }
        if p.option_count > 1 {
            let label = format!(
                "From another wall ({}/{})",
                p.option_idx + 1,
                p.option_count
            );
            actions.push(
                Action::new(
                    label,
                    "Measure the distance from a different outline wall",
                    |_| SketchEvent::CyclePositionReference,
                )
                .bare(),
            );
        }
        show_box(
            ctx,
            rect,
            &title,
            None,
            "e.g. 3.5",
            view.position_error,
            view.position_input,
            &actions,
            on_event,
        );
        return;
    }

    // ── Post-drag distance (Move tool) ──────────────────────────────────────
    if let Some(p) = view.edit_drag_prompt.as_ref() {
        let sub = format!(
            "currently {:.2}, option {}/{}",
            p.current_distance,
            p.option_idx + 1,
            p.option_count
        );
        let actions = vec![
            Action::new("Apply", "Apply this distance", SketchEvent::ApplyEditDrag).primary(),
            Action::new(
                "Different measurement",
                "Try another reference wall",
                |_| SketchEvent::CycleEditDragMeasurement,
            )
            .bare(),
            Action::new(
                "Cancel",
                "Revert the wall to its position before this drag",
                |_| SketchEvent::CancelEditDrag,
            )
            .bare()
            .with_clear(),
        ];
        show_box(
            ctx,
            rect,
            "Distance from highlighted reference wall (m)",
            Some(&sub),
            "e.g. 3.5",
            view.edit_drag_error,
            view.edit_drag_input,
            &actions,
            on_event,
        );
        return;
    }

    // ── Roof: flat-top (rect) height ────────────────────────────────────────
    if view.roof_rect_height_pending {
        let actions = vec![
            Action::new(
                "Set",
                "Raise the ceiling area to a flat top at this height",
                |v| SketchEvent::AssignRoofRectHeight(v),
            )
            .primary(),
            roof_rect_cancel(),
        ];
        show_box(
            ctx,
            rect,
            "Flat-top height (m)",
            None,
            "e.g. 4.2",
            view.roof_error,
            view.roof_apex_input,
            &actions,
            on_event,
        );
        return;
    }

    // ── Roof: flat-top (rect) side length ───────────────────────────────────
    if let Some(axis) = view.roof_rect_length_axis {
        let title = match axis {
            X => "Flat-top width, E–W (m)",
            Y => "Flat-top depth, N–S (m)",
        };
        let actions = vec![
            Action::new(
                "Set",
                "Use exactly this side length (the near corner stays put)",
                |v| SketchEvent::AssignRoofRectLength(v),
            )
            .primary()
            .keep_buffer(),
            Action::new("Skip", "Accept the drawn length as-is", |_| {
                SketchEvent::SkipRoofRectLength
            })
            .bare(),
            roof_rect_cancel(),
        ];
        show_box(
            ctx,
            rect,
            title,
            None,
            "e.g. 4.0",
            view.roof_error,
            view.roof_ridge_length_input,
            &actions,
            on_event,
        );
        return;
    }

    // ── Roof: ridge height ──────────────────────────────────────────────────
    if view.roof_ridge_height_pending {
        let actions = vec![
            Action::new(
                "Set",
                "Raise (or lower) the ceiling area to a ridge at this height",
                |v| SketchEvent::AssignRoofRidgeHeight(v),
            )
            .primary(),
            roof_ridge_cancel(),
        ];
        show_box(
            ctx,
            rect,
            "Ridge height (m)",
            None,
            "e.g. 4.2",
            view.roof_error,
            view.roof_apex_input,
            &actions,
            on_event,
        );
        return;
    }

    // ── Roof: ridge length ──────────────────────────────────────────────────
    if view.roof_ridge_length_pending {
        let set_hint = if view.roof_ridge_length_anchored {
            "Use exactly this ridge length (from the highlighted end)"
        } else {
            "Use exactly this ridge length"
        };
        let actions = vec![
            Action::new("Set", set_hint, SketchEvent::AssignRoofRidgeLength)
                .primary()
                .keep_buffer(),
            Action::new("Skip", "Accept the drawn length as-is", |_| {
                SketchEvent::SkipRoofRidgeLength
            })
            .bare(),
            roof_ridge_cancel(),
        ];
        show_box(
            ctx,
            rect,
            "Ridge length (m)",
            None,
            "e.g. 6.0",
            view.roof_error,
            view.roof_ridge_length_input,
            &actions,
            on_event,
        );
        return;
    }

    // ── Roof: apex (single high point) height ───────────────────────────────
    if view.roof_point_height_pending {
        let actions = vec![
            Action::new(
                "Set",
                "Slope the ceiling area up (or down) to this height at the point",
                |v| SketchEvent::AssignRoofApexHeight(v),
            )
            .primary(),
            Action::new("Cancel", "Discard this roof point", |_| {
                SketchEvent::CancelPendingRoofPoint
            })
            .bare(),
        ];
        show_box(
            ctx,
            rect,
            "Apex height (m)",
            None,
            "e.g. 4.2",
            view.roof_error,
            view.roof_apex_input,
            &actions,
            on_event,
        );
        return;
    }

    // ── Floor-step amount (Raise / Lower own the sign) ──────────────────────
    if view.step_pick_pending {
        let actions = vec![
            Action::new("Raise", "Raise this area's floor by the amount", |v| {
                SketchEvent::AssignStepLevel(v.abs())
            }),
            Action::new("Lower", "Lower this area's floor by the amount", |v| {
                SketchEvent::AssignStepLevel(-v.abs())
            }),
            Action::new("Cancel", "Discard this step area", |_| {
                SketchEvent::CancelPendingStep
            })
            .bare()
            .with_clear(),
        ];
        show_box(
            ctx,
            rect,
            "Step amount (m)",
            None,
            "e.g. 0.15",
            view.step_error,
            view.step_amount_input,
            &actions,
            on_event,
        );
        return;
    }

    // ── Ceiling-step amount (Up / Down own the sign) ────────────────────────
    if view.ceiling_step_amount_pending {
        let actions = vec![
            Action::new("Up", "Raise the ceiling on that side by the amount", |v| {
                SketchEvent::AssignCeilingStepDelta(v.abs())
            }),
            Action::new(
                "Down",
                "Lower the ceiling on that side by the amount",
                |v| SketchEvent::AssignCeilingStepDelta(-v.abs()),
            ),
            Action::new(
                "Keep same height",
                "Split without a step — both sides stay the same height",
                |_| SketchEvent::KeepCeilingSameHeight,
            )
            .bare()
            .with_clear(),
            Action::new("Cancel", "Discard this ceiling step", |_| {
                SketchEvent::CancelPendingCeilingStep
            })
            .bare()
            .with_clear(),
        ];
        show_box(
            ctx,
            rect,
            "Ceiling step (m)",
            None,
            "e.g. 0.2",
            view.ceiling_step_error,
            view.ceiling_amount_input,
            &actions,
            on_event,
        );
    }
}

/// The two roof flows share a "Cancel" with distinct events; small helpers keep
/// the prompt arms above terse.
fn roof_rect_cancel() -> Action {
    Action::new("Cancel", "Discard this flat-topped roof", |_| {
        SketchEvent::CancelPendingRoofRect
    })
    .bare()
    .with_clear()
}

fn roof_ridge_cancel() -> Action {
    Action::new("Cancel", "Discard this ridge", |_| {
        SketchEvent::CancelPendingRoofRidge
    })
    .bare()
    .with_clear()
}

/// Draw the floating window: question (title bar / drag handle), running
/// expression, numeric keypad with the big `+`, then the action buttons.
#[allow(clippy::too_many_arguments)]
fn show_box<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    rect: egui::Rect,
    title: &str,
    subtitle: Option<&str>,
    hint: &str,
    error: Option<&str>,
    buf: &mut String,
    actions: &[Action],
    on_event: &mut F,
) {
    egui::Window::new(title)
        // Stable id (the title changes per prompt) so the dragged position is
        // remembered across prompts.
        .id(Id::new("ber_number_entry"))
        .order(egui::Order::Foreground)
        // Default parking spot: down the right edge, below the floating canvas
        // view buttons (top-right) and clear of the centred phase signpost.
        // `constrain_to` keeps it inside the canvas once dragged.
        .default_pos(rect.right_top() + egui::vec2(-280.0, 110.0))
        .pivot(Align2::LEFT_TOP)
        .constrain_to(rect)
        .collapsible(true)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_min_width(220.0);
            if let Some(sub) = subtitle {
                ui.label(RichText::new(sub).color(Color32::from_gray(170)));
            }

            // Running expression: editable so a hardware keyboard still works on
            // desktop; the keypad below edits the same buffer on a tablet.
            let resp = ui.add(
                egui::TextEdit::singleline(buf)
                    .desired_width(KEY * 4.0 + 18.0)
                    .font(egui::TextStyle::Heading)
                    .hint_text(hint),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            ui.add_space(4.0);
            // One row: the digit grid, the stacked +/− column, then the
            // prompt's action buttons (big and colour-coded) down the right.
            let mut fired: Option<&Action> = None;
            ui.horizontal(|ui| {
                digit_grid(ui, buf);
                plus_minus(ui, buf);
                ui.add_space(8.0);
                // Parse AFTER the keypad edits so a just-pressed key enables the
                // action buttons this same frame.
                let parsed = parse_dimension_expression(buf);
                ui.vertical(|ui| {
                    for a in actions {
                        let enabled = !a.needs_value || parsed.is_some();
                        if action_button(ui, a, enabled) {
                            fired = Some(a);
                        }
                    }
                });
            });

            if let Some(err) = error {
                ui.label(RichText::new(format!("⚠ {err}")).color(Color32::from_rgb(255, 120, 90)));
            }

            let parsed = parse_dimension_expression(buf);
            // Enter triggers the primary action when its value requirement is met.
            if enter && fired.is_none() {
                if let Some(a) = actions.iter().find(|a| a.primary) {
                    if !a.needs_value || parsed.is_some() {
                        fired = Some(a);
                    }
                }
            }
            if let Some(a) = fired {
                on_event(UiEvent::Sketch((a.event)(parsed.unwrap_or(0.0))));
                if a.clear {
                    buf.clear();
                }
            }
        });
}

/// The 3-wide digit grid (`7-8-9` / `4-5-6` / `1-2-3` / `0 . Del`). Each press
/// is just a [`Key`] handed to the input engine — this fn owns only the layout,
/// not what a key means.
fn digit_grid(ui: &mut egui::Ui, buf: &mut String) {
    ui.vertical(|ui| {
        for row in [['7', '8', '9'], ['4', '5', '6'], ['1', '2', '3']] {
            ui.horizontal(|ui| {
                for d in row {
                    if digit(ui, &d.to_string()) {
                        apply(buf, Key::Digit(d));
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            if digit(ui, "0") {
                apply(buf, Key::Digit('0'));
            }
            if digit(ui, ".") {
                apply(buf, Key::Dot);
            }
            if digit(ui, "Del") {
                apply(buf, Key::Backspace);
            }
        });
    });
}

/// The operator column beside the digits: **+** on the top half, **−** on the
/// bottom half, each spanning two key rows. `+` adds the next length to the
/// running total; `−` subtracts it.
fn plus_minus(ui: &mut egui::Ui, buf: &mut String) {
    let half = egui::vec2(KEY * 1.5, KEY * 2.0);
    ui.vertical(|ui| {
        if ui
            .add(op_button("+", Color32::from_rgb(46, 110, 70), half))
            .hint("Add another length to the running total")
            .clicked()
        {
            apply(buf, Key::Plus);
        }
        if ui
            .add(op_button("−", Color32::from_rgb(150, 80, 50), half))
            .hint("Subtract the next length from the running total")
            .clicked()
        {
            apply(buf, Key::Minus);
        }
    });
}

/// One keypad digit/Del key.
fn digit(ui: &mut egui::Ui, label: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(label).size(18.0)).min_size(egui::vec2(KEY, KEY)))
        .clicked()
}

/// A filled operator key (`+` / `−`) sized to span half the digit grid.
fn op_button(label: &str, fill: Color32, size: egui::Vec2) -> egui::Button<'static> {
    egui::Button::new(RichText::new(label).size(30.0).color(Color32::WHITE))
        .min_size(size)
        .fill(fill)
}

/// A prompt action button down the right of the box: bigger than a keypad key
/// and colour-coded by role — green to commit the value (Apply / Set / the
/// primary), blue for an alternate value (Raise / Lower / Up / Down), amber for
/// the value-free arms (Skip / Cancel / Centre / …).
fn action_button(ui: &mut egui::Ui, a: &Action, enabled: bool) -> bool {
    let fill = if a.primary {
        Color32::from_rgb(46, 125, 70)
    } else if a.needs_value {
        Color32::from_rgb(48, 96, 158)
    } else {
        Color32::from_rgb(150, 110, 40)
    };
    let btn = egui::Button::new(RichText::new(&a.label).size(16.0).color(Color32::WHITE))
        .min_size(egui::vec2(128.0, KEY))
        .fill(fill);
    ui.add_enabled(enabled, btn).hint(a.hint).clicked()
}
