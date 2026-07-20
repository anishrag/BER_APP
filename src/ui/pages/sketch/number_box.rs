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

use std::time::Duration;

use egui::{Align2, Color32, Id, RichText};

use super::{SketchEvent, SketchView};
use crate::controller::app_controller::{
    ComplexWindowPlacement, DoorSidelightsMode, DoorSidelightsPlacement, ExistingWindowType,
    WindowTypeChoice,
};
use crate::number_entry::bluetooth::{LaserSource, Status};
use crate::number_entry::{apply_over_suggestion, parse_dimension_expression, Key};
use crate::ui::pages::events::UiEvent;
use crate::ui::widgets::HintExt;
use crate::ui::widgets::{photo_button_pair, text_input, PhotoQuality};
use ber_model::ber::common::ItemId;
use ber_model::ber::fenestration::{
    window_shape_area, window_shape_from_inputs, ComplexWindowKind, DoorKind, Frame, Glazing,
    Overshading, WindowTopShape,
};
use ber_model::ber::openings::SidelightLayout;

/// Minimum finger-target size for keypad keys (mirrors the on-screen pads).
const KEY: f32 = 46.0;

/// egui temp-memory id holding the multi-field box's focused-field index, shared
/// by [`show_box_multi`] and the openings branch (which resets it per placement).
const FOCUS_MEM: &str = "ber_number_entry_focus";

/// How the assessor feeds a measurement into the box: the on-screen keypad, or
/// a paired Bluetooth laser meter. A per-page preference (kept on
/// [`super::categories::CategoryUiState`]) so it persists across prompts.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// The numeric keypad with the +/− operator column.
    #[default]
    Keypad,
    /// A Leica DISTO laser meter; readings stream in over BLE.
    Laser,
}

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
    /// Clear the expression buffer(s) after firing (the prefilled prompts —
    /// position, roof lengths — keep their value instead).
    clear: bool,
    /// Additionally gated by the box's `extra_valid` flag — used by the openings
    /// Place/Fill actions so they're disabled until the window-type name is given.
    gated: bool,
    /// Builds the event from the parsed field values (index-aligned with the
    /// box's fields; `None` where a field is blank/unparsable). Single-value
    /// prompts get a one-element slice.
    event: Box<dyn Fn(&[Option<f32>]) -> SketchEvent>,
}

impl Action {
    /// A single-value action — the common case. The closure gets the one field's
    /// value (0.0 if somehow blank, but `needs_value` gates that away).
    fn new(
        label: impl Into<String>,
        hint: &'static str,
        event: impl Fn(f32) -> SketchEvent + 'static,
    ) -> Self {
        Action::new_multi(label, hint, move |vs| {
            event(vs.first().copied().flatten().unwrap_or(0.0))
        })
    }
    /// A multi-field action — the closure gets every field's parsed value.
    fn new_multi(
        label: impl Into<String>,
        hint: &'static str,
        event: impl Fn(&[Option<f32>]) -> SketchEvent + 'static,
    ) -> Self {
        Action {
            label: label.into(),
            hint,
            needs_value: true,
            primary: false,
            clear: true,
            gated: false,
            event: Box::new(event),
        }
    }
    fn primary(mut self) -> Self {
        self.primary = true;
        self
    }
    /// Also require the box's `extra_valid` flag (e.g. window-type name present).
    fn gated(mut self) -> Self {
        self.gated = true;
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
    use manip_plan::ParameterAxis::{X, Y};

    laser_autodetect(ctx, view);

    // ── Window/door placement: size entry in ONE multi-field box ─────────────
    // Width + Height (and the door+sidelights lights) live together in the same
    // floating box, sharing the keypad / laser — a laser shot fills the focused
    // field then jumps to the next. Openings mode is exclusive with the plan
    // prompts below, so this branch is safe to take first.
    if let Some(prompt) = view.opening_fields_prompt.as_ref() {
        let title = prompt.title.clone();
        let error = view.opening_dim_error;
        // Each tool group has its own Overshading DEFAULT (rooflights: "Very
        // little" — they look at the sky, so obstacles rarely shade them). The
        // pick persists across placements (repeat windows are one pick), so
        // reseed ONLY when the default itself changes — switching tool groups
        // — never on every pick. Runs BEFORE the per-seq block so an edit
        // prompt's prefill (below) wins over the reseed.
        let ov_id = Id::new("ber_opening_default_overshading");
        if ctx.data(|d| d.get_temp::<Overshading>(ov_id)) != Some(prompt.default_overshading) {
            ctx.data_mut(|d| d.insert_temp(ov_id, prompt.default_overshading));
            *view.overshading = prompt.default_overshading;
        }
        // One-time defaults for the state that outlives placements: opes start
        // at 1 (the common single-sash case) and a NEW type starts draught-
        // stripped (modern gasketed units — untick for old sashes). Seeded once
        // per app run so the assessor's own picks persist thereafter.
        let seeded_id = Id::new("ber_opening_defaults_seeded");
        if !ctx.data(|d| d.get_temp::<bool>(seeded_id).unwrap_or(false)) {
            ctx.data_mut(|d| d.insert_temp(seeded_id, true));
            *view.wt_draught_stripped = true;
            *view.door_draught_stripped = true;
            if view.opening_num_opes.trim().is_empty() {
                *view.opening_num_opes = "1".to_string();
            }
        }
        // A fresh host pick (new seq) starts the box on its first field, so the
        // keypad/laser don't keep editing whichever field the last placement left
        // focused.
        let seq_id = Id::new("ber_opening_seq");
        if ctx.data(|d| d.get_temp::<u64>(seq_id)) != Some(prompt.seq) {
            ctx.data_mut(|d| {
                d.insert_temp(seq_id, prompt.seq);
                d.insert_temp(Id::new(FOCUS_MEM), 0usize);
            });
            // A new placement starts with the door+sidelights toggles cleared and
            // both sidelights present (Overall mode's default = even split).
            *view.sidelight_left_has_sill = false;
            *view.sidelight_right_has_sill = false;
            *view.transom_semicircular = false;
            *view.sidelight_left_present = true;
            *view.sidelight_right_present = true;
            // …and with the Full width / Full height toggles off (each pick —
            // or edit — starts from typed measurements).
            *view.opening_full_width = false;
            *view.opening_full_height = false;
            // Photos are per-window, so a fresh placement starts with none (unlike
            // the window type / overshading, which persist across placements).
            view.pending_window_photos.clear();
            // Edit mode: seed the box from the opening being edited — its size
            // into the W/H fields, plus its type-specific attributes. Once, per
            // prompt (seq).
            let fmt = |v: f32| {
                let s = format!("{v:.3}");
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            };
            // A window (wall or roof): size + shared type + overshading + opes
            // (+ the tickable top for a wall window).
            if let Some(edit) = prompt.edit {
                *view.opening_width_input = fmt(edit.width_m);
                *view.opening_height_input = fmt(edit.height_m);
                *view.wt_existing_id = Some(edit.window_type_id);
                *view.overshading = edit.overshading;
                *view.opening_num_opes = edit
                    .num_opes
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "1".to_string());
                // Seed the shaped-top tick from the window's stored shape (a
                // complex/roof shape hides the tick — nothing to seed). Like the
                // overshading seed above, this overwrites the pinned placement
                // pick: the edited window's state becomes the "last used".
                if let Some(top) = edit.top {
                    view.cat_state.window_top_shape = top;
                }
            }
            // A door: size + its type name / kind / draught-stripped flag.
            if let Some(edit) = &prompt.door_edit {
                *view.opening_width_input = fmt(edit.width_m);
                *view.opening_height_input = fmt(edit.height_m);
                *view.door_type_name_input = edit.type_name.clone();
                *view.door_kind = edit.kind.clone();
                *view.door_draught_stripped = edit.draught_stripped;
            }
        }
        // The toggles are captured into the Place event by value (the checkboxes
        // below mutate them for the NEXT frame; a Place click is always a separate
        // interaction, so it reads the settled values). A sidelight sill only
        // counts when its box is ticked.
        let semicircular = *view.transom_semicircular;
        let (left_has_sill, right_has_sill) = (
            *view.sidelight_left_has_sill,
            *view.sidelight_right_has_sill,
        );
        // Door+sidelights input mode + (Overall) which sides carry glass. `None`
        // for every other tool. Captured by value into the Place event.
        let ds_mode = prompt.door_sidelights;
        let (left_present, right_present) =
            (*view.sidelight_left_present, *view.sidelight_right_present);
        // Complex-window shape kind (`None` for every other tool), captured by
        // value into the Place event. A live area readout of the current dims is
        // shown as the box subtitle.
        let cx_kind = prompt.complex_window;
        // The plain Window tool's shaped-top tick — `Some(settled pick)` when
        // offered (plain Window placement, or editing a tickable window),
        // captured by value into the Place / Apply events like `ds_mode`.
        let win_top = prompt.window_top;
        // While a shaped top is ticked, spell out the height semantics.
        let top_note = matches!(win_top, Some(t) if t != WindowTopShape::Flat)
            .then_some("Height = TOTAL at the centre (apex); width at the bottom");
        let cx_area_label = cx_kind.map(|kind| {
            let d = |s: &str| parse_dimension_expression(s).unwrap_or(0.0);
            let dims = [
                d(view.opening_width_input),
                d(view.opening_height_input),
                d(view.window_shape_c_input),
                d(view.window_shape_d_input),
            ];
            let n = kind.field_labels().len().min(dims.len());
            let (w, h, shape) = window_shape_from_inputs(kind, &dims[..n]);
            format!("area ≈ {:.2} m²", window_shape_area(shape, w, h))
        });
        // Full width / Full height (plain Window, Door, or door+sidelights on
        // a wall — the prompt carries the resolved spans): ticking a box
        // disables its field, which then displays the wall span the opening
        // will take. The flags ride the Place / Apply events by value (settled
        // previous-frame values, like the other toggles); the controller
        // re-resolves the spans against the LIVE wall when it places. The
        // COMPOSITE offers them in its Overall input mode only — there the
        // whole-doorset W/H fields map 1:1 onto the wall spans, while Detailed
        // mode types every piece explicitly.
        let ds_detailed = ds_mode == Some(DoorSidelightsMode::Detailed);
        let full_span = prompt.full_span;
        let offer_full = full_span.is_some() && !ds_detailed;
        let full_width = offer_full && *view.opening_full_width;
        let full_height = offer_full && *view.opening_full_height;
        let fmt_span = |v: f32| {
            let s = format!("{v:.3}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        };
        let mut full_width_display = full_span
            .map(|i| fmt_span(i.wall_len_m))
            .unwrap_or_default();
        let mut full_height_display = full_span
            .map(|i| fmt_span(i.face_height_m))
            .unwrap_or_default();
        // Documents the split rules while a composite full-span tick is on:
        // Full width — door width typed, sidelights share the rest; Full
        // height — door height typed, sidelights run to the roofline and the
        // transom fills the strip left above the door.
        let ds_full_note = (ds_mode.is_some() && (full_width || full_height)).then(
            || match (full_width, full_height) {
                (true, true) => {
                    "Whole wall + face: type the door size — the sidelight(s) share the remaining width; the transom tops the door. Needs an empty wall."
                }
                (true, false) => {
                    "Full width: type the door width — the sidelight(s) share the rest of the wall. No end margins; needs an empty wall."
                }
                _ => "Full height: sidelights run floor → roofline; the transom fills the strip above the typed door.",
            },
        );
        // The window-type choice settled on a previous frame (the picker below
        // mutates it for the NEXT frame; a Place click reads the settled value),
        // captured by value into the action events.
        let window_type = current_window_type(view);
        let overshading = *view.overshading;
        let num_opes = view.opening_num_opes.trim().parse::<u32>().ok();
        let door_draught_stripped = *view.door_draught_stripped;
        // "Edit door" attributes, captured by value into the ApplyDoorEdit event.
        let is_door_edit = prompt.door_edit.is_some();
        let door_type_name = view.door_type_name_input.clone();
        let door_kind = view.door_kind.clone();
        // Photos picked so far for this placement, captured by value into the
        // events (the photo button below adds to the list for a later frame).
        let photos = view.pending_window_photos.clone();
        let place_window_type = window_type.clone();
        let place_photos = photos.clone();
        // Edit mode: the primary action APPLIES the resize/retype to the
        // existing opening instead of placing a new one. A DOOR edit emits its
        // own event (door type/kind/draught); a window (wall or roof) reuses
        // ApplyOpeningEdit (the controller dispatches on what's being edited).
        let primary_action = if is_door_edit {
            let apply_photos = photos.clone();
            Action::new_multi(
                "Apply",
                "Apply the new size / type to this door",
                move |vs| {
                    let g = |i: usize| vs.get(i).copied().flatten();
                    SketchEvent::ApplyDoorEdit {
                        width_m: g(0).unwrap_or(0.0),
                        height_m: g(1).unwrap_or(0.0),
                        type_name: door_type_name.clone(),
                        kind: door_kind.clone(),
                        draught_stripped: door_draught_stripped,
                        photos: apply_photos.clone(),
                    }
                },
            )
        } else if prompt.edit.is_some() {
            let apply_window_type = window_type.clone();
            let apply_photos = photos.clone();
            Action::new_multi(
                "Apply",
                "Apply the new size / type to this window",
                move |vs| {
                    let g = |i: usize| vs.get(i).copied().flatten();
                    SketchEvent::ApplyOpeningEdit {
                        width_m: g(0).unwrap_or(0.0),
                        height_m: g(1).unwrap_or(0.0),
                        full_width,
                        full_height,
                        window_top: win_top,
                        window_type: apply_window_type.clone(),
                        overshading,
                        num_opes,
                        photos: apply_photos.clone(),
                    }
                },
            )
        } else {
            Action::new_multi("Place", "Place this opening", move |vs| {
                let g = |i: usize| vs.get(i).copied().flatten();
                // Fields 0/1 are the door leaf's W/H — except in Overall mode, where
                // the whole-doorset W/H come first and the door leaf shifts to 2/3.
                let (width_m, height_m) = match ds_mode {
                    Some(DoorSidelightsMode::Overall) => (g(2).unwrap_or(0.0), g(3).unwrap_or(0.0)),
                    _ => (g(0).unwrap_or(0.0), g(1).unwrap_or(0.0)),
                };
                let door_sidelights = match ds_mode {
                    None => None,
                    // Overall: [overall W, overall H, door W, door H, sill]. The two
                    // present-checkboxes pick which side(s) the leftover width fills;
                    // neither ticked falls back to an even split.
                    Some(DoorSidelightsMode::Overall) => {
                        let layout = match (left_present, right_present) {
                            (true, false) => SidelightLayout::LeftOnly,
                            (false, true) => SidelightLayout::RightOnly,
                            _ => SidelightLayout::Both,
                        };
                        Some(DoorSidelightsPlacement::Overall {
                            overall_w: g(0).unwrap_or(0.0),
                            overall_h: g(1).unwrap_or(0.0),
                            layout,
                            sill: if left_has_sill { g(4) } else { None },
                            transom_semicircular: semicircular,
                        })
                    }
                    // Detailed: [door W, door H, left W, left sill, right W, right
                    // sill, transom H]. A sidelight sill only counts when ticked.
                    Some(DoorSidelightsMode::Detailed) => Some(DoorSidelightsPlacement::Detailed {
                        left: g(2),
                        left_sill: if left_has_sill { g(3) } else { None },
                        right: g(4),
                        right_sill: if right_has_sill { g(5) } else { None },
                        transom: g(6),
                        transom_semicircular: semicircular,
                    }),
                };
                // A complex window carries its shape kind + the ordered dim inputs;
                // the controller derives the bounding box + shape from them.
                let complex_window = cx_kind.map(|kind| {
                    let n = kind.field_labels().len();
                    let dims = (0..n).map(|i| g(i).unwrap_or(0.0)).collect();
                    ComplexWindowPlacement { kind, dims }
                });
                SketchEvent::PlaceOpening {
                    width_m,
                    height_m,
                    full_width,
                    full_height,
                    door_sidelights,
                    complex_window,
                    window_top: win_top,
                    window_type: place_window_type.clone(),
                    overshading,
                    num_opes,
                    door_draught_stripped,
                    photos: place_photos.clone(),
                }
            })
        };
        let mut actions = vec![primary_action.primary().gated()];
        if prompt.offer_full_surface {
            let fill_window_type = window_type.clone();
            let fill_photos = photos.clone();
            actions.push(
                Action::new(
                    "Fill whole surface",
                    "Make the whole wall one window (no size needed)",
                    move |_| SketchEvent::FillOpeningSurface {
                        window_type: fill_window_type.clone(),
                        overshading,
                        num_opes,
                        photos: fill_photos.clone(),
                    },
                )
                .bare()
                .with_clear()
                .gated(),
            );
        }
        actions.push(
            Action::new("Cancel", "Abandon this placement", |_| {
                SketchEvent::CancelOpeningPlacement
            })
            .bare()
            .with_clear(),
        );
        // Bind each field to its own buffer (disjoint borrows of distinct view
        // fields). Width + Height are always present; the door+sidelights extras
        // (left W, left sill, right W, right sill, transom H) only when listed.
        let f = &prompt.fields;
        // Width/Height: when a Full toggle is on, the field is disabled and
        // shows the resolved span (a per-frame display string — the typed
        // buffer is kept, so unticking restores it). The toggle itself is the
        // inline checkbox beside the field, offered only with a `full_span`.
        let mut box_fields = vec![
            BoxField {
                label: &f[0].label,
                hint: f[0].hint,
                buf: if full_width {
                    &mut full_width_display
                } else {
                    &mut *view.opening_width_input
                },
                required: f[0].required,
                enabled: !full_width,
                measurable: true,
                check: offer_full.then(|| BoxCheck {
                    label: "Full width",
                    value: &mut *view.opening_full_width,
                }),
            },
            BoxField {
                label: &f[1].label,
                hint: f[1].hint,
                buf: if full_height {
                    &mut full_height_display
                } else {
                    &mut *view.opening_height_input
                },
                required: f[1].required,
                enabled: !full_height,
                measurable: true,
                check: offer_full.then(|| BoxCheck {
                    label: "Full height",
                    value: &mut *view.opening_full_height,
                }),
            },
        ];
        // The door+sidelights extra fields, bound by mode. Fields 0/1 (above) are
        // the whole-doorset W/H in Overall mode, or the door leaf W/H otherwise.
        match ds_mode {
            Some(DoorSidelightsMode::Overall) => {
                // [.., door W, door H, sill]. Door leaf under the overall W/H.
                box_fields.push(BoxField {
                    label: &f[2].label,
                    hint: f[2].hint,
                    buf: &mut *view.door_width_input,
                    required: true,
                    enabled: true,
                    measurable: true,
                    check: None,
                });
                // The door LEAF height stays TYPED even under Full height —
                // the transom fills whatever strip the door leaves below the
                // face (only the overall H above locks to the face).
                box_fields.push(BoxField {
                    label: &f[3].label,
                    hint: f[3].hint,
                    buf: &mut *view.door_height_input,
                    required: true,
                    enabled: true,
                    measurable: true,
                    check: None,
                });
                // One sill for the whole assembly — active only when ticked.
                // Reuses the left-sill buffer + the (repurposed) "has a sill"
                // flag. Full height forces sill 0, so it deactivates too.
                box_fields.push(BoxField {
                    label: &f[4].label,
                    hint: f[4].hint,
                    buf: &mut *view.sidelight_left_sill_input,
                    required: false,
                    enabled: left_has_sill && !full_height,
                    measurable: true,
                    check: None,
                });
            }
            Some(DoorSidelightsMode::Detailed) => {
                // [.., left W, left sill, right W, right sill, transom H].
                box_fields.push(BoxField {
                    label: &f[2].label,
                    hint: f[2].hint,
                    buf: &mut *view.sidelight_left_input,
                    required: false,
                    enabled: true,
                    measurable: true,
                    check: None,
                });
                // Left sill — only active when its box is ticked.
                box_fields.push(BoxField {
                    label: &f[3].label,
                    hint: f[3].hint,
                    buf: &mut *view.sidelight_left_sill_input,
                    required: false,
                    enabled: left_has_sill,
                    measurable: true,
                    check: None,
                });
                box_fields.push(BoxField {
                    label: &f[4].label,
                    hint: f[4].hint,
                    buf: &mut *view.sidelight_right_input,
                    required: false,
                    enabled: true,
                    measurable: true,
                    check: None,
                });
                // Right sill — only active when its box is ticked.
                box_fields.push(BoxField {
                    label: &f[5].label,
                    hint: f[5].hint,
                    buf: &mut *view.sidelight_right_sill_input,
                    required: false,
                    enabled: right_has_sill,
                    measurable: true,
                    check: None,
                });
                box_fields.push(BoxField {
                    label: &f[6].label,
                    hint: f[6].hint,
                    buf: &mut *view.transom_input,
                    required: false,
                    enabled: true,
                    measurable: true,
                    check: None,
                });
            }
            // Not door+sidelights. A complex window binds its 3rd/4th dims to the
            // shape buffers (fields 0/1 already took width/height above); a plain
            // window/door has only the two fields, so this is a no-op there.
            None => {
                if f.len() > 2 {
                    box_fields.push(BoxField {
                        label: &f[2].label,
                        hint: f[2].hint,
                        buf: &mut *view.window_shape_c_input,
                        required: f[2].required,
                        enabled: true,
                        measurable: true,
                        check: None,
                    });
                }
                if f.len() > 3 {
                    box_fields.push(BoxField {
                        label: &f[3].label,
                        hint: f[3].hint,
                        buf: &mut *view.window_shape_d_input,
                        required: f[3].required,
                        enabled: true,
                        measurable: true,
                        check: None,
                    });
                }
            }
        }
        // A NEW window type's YEAR + GAP: typed, non-measurable values. As box
        // fields they're driven by the box's OWN keypad (no second pad), and the
        // laser auto-advance skips them. Shown only when defining a new type.
        if prompt.offer_window_type && view.wt_existing_id.is_none() {
            box_fields.push(BoxField {
                label: "year",
                hint: "2010",
                buf: &mut *view.wt_year_input,
                required: false,
                enabled: true,
                measurable: false,
                check: None,
            });
            box_fields.push(BoxField {
                label: "gap",
                hint: "mm",
                buf: &mut *view.wt_gap_input,
                required: false,
                enabled: true,
                measurable: false,
                check: None,
            });
        }
        let mut checks = Vec::new();
        match ds_mode {
            Some(DoorSidelightsMode::Overall) => {
                // Which side(s) the leftover width fills, one shared sill, and the
                // (auto) transom's shape.
                checks.push(BoxCheck {
                    label: "left sidelight",
                    value: &mut *view.sidelight_left_present,
                });
                checks.push(BoxCheck {
                    label: "right sidelight",
                    value: &mut *view.sidelight_right_present,
                });
                checks.push(BoxCheck {
                    label: "has a sill",
                    value: &mut *view.sidelight_left_has_sill,
                });
                checks.push(BoxCheck {
                    label: "semicircular transom",
                    value: &mut *view.transom_semicircular,
                });
            }
            Some(DoorSidelightsMode::Detailed) => {
                checks.push(BoxCheck {
                    label: "left sidelight has a sill",
                    value: &mut *view.sidelight_left_has_sill,
                });
                checks.push(BoxCheck {
                    label: "right sidelight has a sill",
                    value: &mut *view.sidelight_right_has_sill,
                });
                checks.push(BoxCheck {
                    label: "semicircular transom",
                    value: &mut *view.transom_semicircular,
                });
            }
            None => {}
        }
        // The window-type picker (for window-bearing tools), rendered in the box
        // body. Captures the wt_* state fields (disjoint from the box fields).
        let existing_types = view.existing_window_types.clone();
        let offer_wt = prompt.offer_window_type;
        let mut picker = |ui: &mut egui::Ui| {
            // "Edit door" box: the door has no window-type picker — instead it
            // collects its own type name / kind / draught-stripped flag.
            if is_door_edit {
                ui.horizontal(|ui| {
                    ui.label("type").hint("A name for this door type");
                    text_input(ui, view.door_type_name_input, KEY, "Door");
                });
                door_kind_picker(ui, &mut *view.door_kind);
                ui.checkbox(view.door_draught_stripped, "draught stripped");
                window_photo_picker(ui, &mut *view.pending_window_photos, false);
                return;
            }
            if !offer_wt {
                return;
            }
            window_type_picker(
                ui,
                &existing_types,
                &mut *view.wt_existing_id,
                &mut *view.wt_glazing,
                &mut *view.wt_frame,
                &mut *view.wt_certified,
                &mut *view.wt_draught_stripped,
                &mut *view.wt_name_input,
            );
            // Overshading is per-WINDOW (not per type), so it shows for every
            // window placement regardless of the type choice above.
            overshading_picker(ui, &mut *view.overshading);
            // How many opening sections this window has (DEAP's ventilation
            // openings count). Persists across placements like the type.
            ui.horizontal(|ui| {
                ui.label("opes")
                    .hint("Number of opening sections in this window");
                text_input(ui, view.opening_num_opes, KEY, "1");
            });
            // Doors count as one opening each; stripped or not is per door.
            // A plain solid Door is the one tool with no window-type picker;
            // door+sidelights announces itself via its mode switch.
            if prompt.door_sidelights.is_some() || !prompt.offer_window_type {
                ui.checkbox(view.door_draught_stripped, "door draught stripped");
            }
            // Photos belong to the GROUP (window type). At least one is REQUIRED
            // when defining a new group; for an existing group adding more is
            // optional (it already has its own).
            let photo_required = view.wt_existing_id.is_none();
            window_photo_picker(ui, &mut *view.pending_window_photos, photo_required);
        };
        // A NEW window type (group) must be named AND have at least one photo —
        // both belong to the group, required only when creating it. Reusing an
        // existing group, or a non-window tool, needs nothing extra. Gates Place.
        let extra_valid = match &window_type {
            WindowTypeChoice::Existing(_) => true,
            WindowTypeChoice::New { name, .. } => {
                !prompt.offer_window_type || (name.is_some() && !photos.is_empty())
            }
        };
        // The Overall/Detailed mode switch — only for the Door + sidelights tool,
        // drawn at the top of the box (above the fields it reshapes).
        let ds_mode_toggle: Option<&mut DoorSidelightsMode> = prompt
            .door_sidelights
            .is_some()
            .then_some(&mut view.cat_state.door_sidelights_mode);
        // The shape dropdown — only for the Complex-window tool, at the box top.
        let cx_kind_toggle: Option<&mut ComplexWindowKind> = prompt
            .complex_window
            .is_some()
            .then_some(&mut view.cat_state.complex_window_kind);
        // The shaped-top 3-way tick — plain Window placement, or editing a
        // window whose shape is tickable. Persists on `CategoryUiState` like
        // the complex-shape dropdown.
        let win_top_toggle: Option<&mut WindowTopShape> = win_top
            .is_some()
            .then_some(&mut view.cat_state.window_top_shape);
        // Edit mode pre-fills the W/H with the window's current size — arm the
        // pristine guard (keyed per prompt seq) so the first key REPLACES the
        // prefill. Placements stay user-entered (no suggestion, never pristine).
        let edit_token = (prompt.edit.is_some() || prompt.door_edit.is_some())
            .then(|| format!("edit-window-{}", prompt.seq));
        show_box_multi(
            ctx,
            rect,
            &title,
            cx_area_label.as_deref().or(ds_full_note).or(top_note),
            error,
            &mut box_fields,
            &mut checks,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            ds_mode_toggle,
            cx_kind_toggle,
            win_top_toggle,
            view.laser,
            &mut picker,
            extra_valid,
            // Placement fields are user-entered W/H (no suggestion); the edit
            // box pre-fills them, so it passes its pristine token.
            edit_token.as_deref(),
            on_event,
        );
        return;
    }

    // ── Wall-face partition cut dimension ────────────────────────────────────
    // Routes through the SAME floating box / keypad / laser as every other
    // single-value prompt, so a laser reading drops straight into a cut
    // measurement. Partition mode is exclusive with the plan prompts below, so
    // this branch is safe to take first.
    if let Some(prompt) = view.partition_view.and_then(|pv| pv.dim_prompt.as_ref()) {
        let actions = vec![
            Action::new("Apply", "Apply this measurement to the cut", |v| {
                SketchEvent::PartitionDimEntry(v)
            })
            .primary(),
            Action::new(
                "Skip",
                "Keep the drawn position for this measurement",
                |_| SketchEvent::PartitionDimSkip,
            )
            .bare()
            .with_clear(),
        ];
        show_box(
            ctx,
            rect,
            &prompt.label,
            None,
            "e.g. 1.2",
            None,
            view.partition_dim_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            None,
            on_event,
        );
        return;
    }

    // ── Default ceiling height (opened from the Heights tool button) ─────────
    if view.ceiling_entry_open {
        let sub = view
            .default_ceiling_height
            .map(|h| format!("currently {h:.2}"));
        let actions = vec![
            Action::new(
                "Set",
                "Set the default ceiling height",
                SketchEvent::SetDefaultCeilingHeight,
            )
            .primary()
            .keep_buffer(),
            Action::new("Close", "Close without changing", |_| {
                SketchEvent::CloseCeilingHeightEntry
            })
            .bare(),
        ];
        show_box(
            ctx,
            rect,
            "Default ceiling height (m)",
            sub.as_deref(),
            "e.g. 2.4",
            view.ceiling_height_error,
            &mut view.cat_state.ceiling_height_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            None,
            on_event,
        );
        return;
    }

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
            Action::new("Use 0.25", "Accept the typical 0.25 m slab", |_| {
                SketchEvent::AssignFloorThickness(0.25)
            })
            .bare(),
        ];
        show_box(
            ctx,
            rect,
            "Floor thickness to the storey below (m)",
            Some("Drawing is paused until the thickness is set."),
            "e.g. 0.25",
            view.new_storey_error,
            view.new_storey_thickness_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            // Pre-filled with the typical 0.25 m — the first typed digit replaces it.
            Some("thickness"),
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
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            None,
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
        // 'Centre between 2 points' pick sub-mode: the canvas is taking two
        // taps — swap the distance keypad for the instruction + a Cancel.
        if view.centre_between_picking {
            let dir_word = match p.axis {
                X => "horizontally",
                Y => "vertically",
            };
            let which = if view.centre_between_first.is_some() {
                "Tap the SECOND point."
            } else {
                "Tap the FIRST point."
            };
            let title = "Centre between 2 points".to_string();
            let sub = format!(
                "Tap two points; the storey will centre between them {dir_word}. \
                 {which} Each tap snaps to the nearest corner or wall."
            );
            let actions = vec![Action::new(
                "Cancel",
                "Stop picking and keep the position prompt",
                |_| SketchEvent::CancelCentreBetween,
            )
            .bare()];
            show_box(
                ctx,
                rect,
                &title,
                Some(&sub),
                "",
                view.position_error,
                view.position_input,
                &actions,
                &mut view.cat_state.input_mode,
                &mut view.cat_state.input_mode_pinned,
                view.laser,
                None,
                on_event,
            );
            return;
        }
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
                Action::new("Centre", "Centre it within the available area", |_| {
                    SketchEvent::CentreRoofPosition
                })
                .bare(),
            );
        }
        if p.offers_centre_between {
            actions.push(
                Action::new(
                    "Centre between 2 points",
                    "Tap two points; the storey centres between them on this axis",
                    |_| SketchEvent::EnterCentreBetween,
                )
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
        // The box is pre-filled with the drawn distance; a new prompt IDENTITY
        // (axis + kind + reference option) means a fresh suggestion, so the first
        // typed digit replaces it. Mirrors app.rs's `position_prompt_key`.
        let sug = format!("pos|{:?}|{:?}|{}", p.axis, p.kind, p.option_idx);
        show_box(
            ctx,
            rect,
            &title,
            None,
            "e.g. 3.5",
            view.position_error,
            view.position_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            Some(sug.as_str()),
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
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            None,
            on_event,
        );
        return;
    }

    // ── Roof: DORMER flat-top height ────────────────────────────────────────
    // The dormer reuses the rect FSM, so its height stage IS
    // `ceiling_rect_height_pending`; the `ceiling_rect_is_dormer` discriminator
    // reroutes the title + Set event so the surrounding roof isn't resloped.
    if view.ceiling_rect_height_pending && view.ceiling_rect_is_dormer {
        if view.copy_height_pick_pending {
            show_copy_height_hint(ctx, rect, view.copy_height_error, on_event);
            return;
        }
        let actions = vec![
            Action::new(
                "Set",
                "Raise a flat-topped dormer to this height (the roof around it keeps its slope)",
                SketchEvent::AssignDormerHeight,
            )
            .primary(),
            copy_height_action(),
            ceiling_rect_cancel(),
        ];
        show_box(
            ctx,
            rect,
            "Dormer height (m)",
            None,
            "e.g. 2.6",
            view.roof_error,
            view.ceiling_apex_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            // Its own per-stage pristine token (see the flat-top block), so a
            // rect → dormer transition re-arms the prefill guard.
            Some(format!("roof-dormer-height|{}", view.ceiling_apex_open_seq).as_str()),
            on_event,
        );
        return;
    }

    // ── Roof: flat-top (rect) height ────────────────────────────────────────
    if view.ceiling_rect_height_pending {
        let actions = vec![
            Action::new(
                "Set",
                "Raise the ceiling area to a flat top at this height",
                |v| SketchEvent::AssignCeilingRectHeight(v),
            )
            .primary(),
            ceiling_rect_cancel(),
        ];
        show_box(
            ctx,
            rect,
            "Flat-top height (m)",
            None,
            "e.g. 4.2",
            view.roof_error,
            view.ceiling_apex_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            // Pre-filled with the storey's default ceiling height (all three roof
            // HEIGHT stages share this buffer + one prefill guard). The token is
            // per-STAGE, not "roof-height" for all three, so a stage→stage
            // transition (e.g. apex/rect → ridge) re-arms the pristine guard and
            // the first typed digit REPLACES the prefill instead of appending.
            Some(format!("roof-rect-height|{}", view.ceiling_apex_open_seq).as_str()),
            on_event,
        );
        return;
    }

    // ── Roof: flat-top (rect) side length ───────────────────────────────────
    if let Some(axis) = view.ceiling_rect_length_axis {
        let title = match axis {
            X => "Flat-top width, E–W (m)",
            Y => "Flat-top depth, N–S (m)",
        };
        let actions = vec![
            Action::new(
                "Set",
                "Use exactly this side length (the near corner stays put)",
                |v| SketchEvent::AssignCeilingRectLength(v),
            )
            .primary()
            .keep_buffer(),
            Action::new("Skip", "Accept the drawn length as-is", |_| {
                SketchEvent::SkipCeilingRectLength
            })
            .bare(),
            ceiling_rect_cancel(),
        ];
        // Pre-filled with the drawn side length; the buffer is re-filled when the
        // LenX → LenY axis changes (mirrors app.rs's `ceiling_rect_len_key`), so
        // the token carries the axis.
        let rect_sug = format!("rect-len|{axis:?}");
        show_box(
            ctx,
            rect,
            title,
            None,
            "e.g. 4.0",
            view.roof_error,
            view.ceiling_ridge_length_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            Some(rect_sug.as_str()),
            on_event,
        );
        return;
    }

    // ── Roof: ridge height ──────────────────────────────────────────────────
    if view.ceiling_ridge_height_pending {
        let mut actions = vec![Action::new(
            "Set",
            "Raise (or lower) the ceiling area to a ridge at this height",
            |v| SketchEvent::AssignCeilingRidgeHeight(v),
        )
        .primary()];
        // Only when the ridge abuts a raised neighbour: slope up to meet it flush.
        if view.ceiling_ridge_meet_edge_height.is_some() {
            actions.push(
                Action::new(
                    "Meet other edge",
                    "Slope up to meet the raised area's ceiling",
                    |_| SketchEvent::MeetOtherEdgeRidgeHeight,
                )
                .bare(),
            );
        }
        actions.push(ceiling_ridge_cancel());
        show_box(
            ctx,
            rect,
            "Ridge height (m)",
            None,
            "e.g. 4.2",
            view.roof_error,
            view.ceiling_apex_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            // Pre-filled with the storey's default ceiling height (all three roof
            // HEIGHT stages share this buffer + one prefill guard). Per-STAGE token
            // (see the flat-top block) so apex/rect → ridge re-arms pristine.
            Some(format!("roof-ridge-height|{}", view.ceiling_apex_open_seq).as_str()),
            on_event,
        );
        return;
    }

    // ── Roof: ridge length ──────────────────────────────────────────────────
    if view.ceiling_ridge_length_pending {
        let set_hint = if view.ceiling_ridge_length_anchored {
            "Use exactly this ridge length (from the highlighted end)"
        } else {
            "Use exactly this ridge length"
        };
        let actions = vec![
            Action::new("Set", set_hint, SketchEvent::AssignCeilingRidgeLength)
                .primary()
                .keep_buffer(),
            Action::new("Skip", "Accept the drawn length as-is", |_| {
                SketchEvent::SkipCeilingRidgeLength
            })
            .bare(),
            ceiling_ridge_cancel(),
        ];
        show_box(
            ctx,
            rect,
            "Ridge length (m)",
            None,
            "e.g. 6.0",
            view.roof_error,
            view.ceiling_ridge_length_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            // Pre-filled with the drawn ridge length.
            Some("ridge-length"),
            on_event,
        );
        return;
    }

    // ── Roof: apex (single high point) height ───────────────────────────────
    if view.ceiling_point_height_pending {
        let actions = vec![
            Action::new(
                "Set",
                "Slope the ceiling area up (or down) to this height at the point",
                |v| SketchEvent::AssignCeilingApexHeight(v),
            )
            .primary(),
            Action::new("Cancel", "Discard this roof point", |_| {
                SketchEvent::CancelPendingCeilingPoint
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
            view.ceiling_apex_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            // Pre-filled with the storey's default ceiling height (all three roof
            // HEIGHT stages share this buffer + one prefill guard). Per-STAGE token
            // (see the flat-top block) so a stage→stage transition re-arms pristine.
            Some(format!("roof-apex-height|{}", view.ceiling_apex_open_seq).as_str()),
            on_event,
        );
        return;
    }

    // ── Floor-step amount (Raise / Lower own the sign) ──────────────────────
    if view.step_pick_pending {
        if view.copy_height_pick_pending {
            show_copy_height_hint(ctx, rect, view.copy_height_error, on_event);
            return;
        }
        let actions = vec![
            Action::new("Raise", "Raise this area's floor by the amount", |v| {
                SketchEvent::AssignStepLevel(v.abs())
            }),
            Action::new("Lower", "Lower this area's floor by the amount", |v| {
                SketchEvent::AssignStepLevel(-v.abs())
            }),
            copy_height_action(),
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
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            None,
            on_event,
        );
        return;
    }

    // ── Ceiling-step amount (Up / Down own the sign; "At height" is the
    // ABSOLUTE entry — the typed value is the ceiling height the area ends
    // up at; the controller converts to the stored relative delta). The
    // SAME box serves editing an EXISTING step (tap-selected): the title
    // flips, the subtitle shows the step's current value + the area's
    // resolved height, and a Delete removes the step entirely. ─────────────
    if view.ceiling_step_amount_pending {
        if view.copy_height_pick_pending {
            show_copy_height_hint(ctx, rect, view.copy_height_error, on_event);
            return;
        }
        let editing = view.ceiling_step_editing.is_some();
        let mut actions = vec![
            Action::new("Up", "Raise the ceiling on that side by the amount", |v| {
                SketchEvent::AssignCeilingStepDelta(v.abs())
            }),
            Action::new(
                "Down",
                "Lower the ceiling on that side by the amount",
                |v| SketchEvent::AssignCeilingStepDelta(-v.abs()),
            ),
            Action::new(
                "At height",
                "Set the area's ceiling TO this height (absolute metres above the floor)",
                |v| SketchEvent::AssignCeilingStepAbsolute(v),
            ),
            copy_height_action(),
            Action::new(
                "Keep same height",
                "Split without a height change — both sections stay the same height",
                |_| SketchEvent::KeepCeilingSameHeight,
            )
            .bare()
            .with_clear(),
        ];
        if editing {
            actions.push(
                Action::new(
                    "Remove division",
                    "Remove this division — the sections re-join",
                    |_| SketchEvent::DeleteSelectedCeilingStep,
                )
                .bare()
                .with_clear(),
            );
        }
        actions.push(
            Action::new(
                "Cancel",
                if editing {
                    "Leave this section as it is"
                } else {
                    "Discard this ceiling division"
                },
                |_| SketchEvent::CancelPendingCeilingStep,
            )
            .bare()
            .with_clear(),
        );
        // The reference line: the current step value (editing) and the
        // area's RESOLVED ceiling height — what "At height" measures against.
        let subtitle = match (
            view.ceiling_step_editing.as_ref(),
            view.ceiling_step_current_height,
        ) {
            (Some((delta, _)), Some(h)) if delta.abs() < 1e-3 => Some(format!(
                "Currently: no step (same height) · area at {h:.2} m"
            )),
            (Some((delta, _)), Some(h)) => {
                Some(format!("Currently: {delta:+.2} m · area at {h:.2} m"))
            }
            (None, Some(h)) => Some(format!("Area currently at {h:.2} m")),
            _ => None,
        };
        show_box(
            ctx,
            rect,
            if editing {
                "Edit ceiling section (m)"
            } else {
                "Ceiling section height (m)"
            },
            subtitle.as_deref(),
            "e.g. 0.2",
            view.ceiling_step_error,
            view.ceiling_amount_input,
            &actions,
            &mut view.cat_state.input_mode,
            &mut view.cat_state.input_mode_pinned,
            view.laser,
            None,
            on_event,
        );
    }
}

/// The "Copy from" arm shared by the three height prompts (dormer / ceiling-
/// step / floor-step): a bare action that puts the app in copy-height pick mode
/// so a tap on a known surface copies its height.
fn copy_height_action() -> Action {
    Action::new(
        "Copy from",
        "Copy the height from a known surface — tap a flat area, level ridge, or flat-topped wall",
        |_| SketchEvent::StartCopyHeight,
    )
    .bare()
}

/// The COMPACT pick-mode panel that replaces the keypad while the assessor is
/// tapping a known surface to copy its height. Reuses the number box's window
/// id so it appears where the box was; shows the instruction (+ any rejection
/// error in red) and a Cancel that re-opens the keypad.
fn show_copy_height_hint<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    rect: egui::Rect,
    error: Option<&str>,
    on_event: &mut F,
) {
    egui::Window::new("Copy height")
        .id(Id::new("ber_number_entry"))
        .order(egui::Order::Foreground)
        .default_pos(rect.right_top() + egui::vec2(-280.0, 110.0))
        .pivot(Align2::LEFT_TOP)
        .constrain_to(rect)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Tap a flat area, ridge, or wall to copy its height");
            if let Some(err) = error {
                ui.label(RichText::new(err).color(Color32::from_rgb(220, 80, 80)));
            }
            ui.add_space(6.0);
            if ui
                .button("Cancel")
                .hint("Back to typing the height")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::CancelCopyHeight));
            }
        });
}

/// The two roof flows share a "Cancel" with distinct events; small helpers keep
/// the prompt arms above terse.
fn ceiling_rect_cancel() -> Action {
    Action::new("Cancel", "Discard this flat-topped roof", |_| {
        SketchEvent::CancelPendingCeilingRect
    })
    .bare()
    .with_clear()
}

fn ceiling_ridge_cancel() -> Action {
    Action::new("Cancel", "Discard this ridge", |_| {
        SketchEvent::CancelPendingCeilingRidge
    })
    .bare()
    .with_clear()
}

/// One labelled text field in the floating box. Most prompts have a single one
/// (no label); the window/door placement has several (Width, Height, …) sharing
/// one keypad / laser, with focus moving between them.
struct BoxField<'a> {
    /// Shown to the left of the box; empty for a single-value prompt.
    label: &'a str,
    hint: &'a str,
    buf: &'a mut String,
    /// An action that `needs_value` is enabled only once every REQUIRED field
    /// parses; optional fields (e.g. door+sidelights lights) may stay blank.
    required: bool,
    /// A disabled field is greyed out and can't take keypad/laser focus (e.g. a
    /// sidelight sill until its "sill" box is ticked). Always `true` for the
    /// single-value box.
    enabled: bool,
    /// Whether this value can be LASER-measured (a dimension). `true` for
    /// dimensions (width/height/sidelights); `false` for typed-only values like
    /// a window type's year or glazing gap. A non-measurable field is skipped by
    /// the laser auto-advance, and always shows the keypad (never the laser
    /// panel) when focused — so there's only ever one keypad, the box's own.
    measurable: bool,
    /// An inline checkbox drawn RIGHT OF the field (the openings "Full width" /
    /// "Full height" toggles). `None` for every other field. The caller reacts
    /// to the toggle on the next frame (disable the field, swap its display).
    check: Option<BoxCheck<'a>>,
}

/// A boolean toggle shown under the fields (e.g. the door+sidelights
/// "semicircular transom" option). Most prompts have none.
struct BoxCheck<'a> {
    label: &'a str,
    value: &'a mut bool,
}

/// The window-type choice currently selected in the placement box's picker — an
/// existing shared type (when its id is still in the list) or a new attribute set
/// from the glazing/frame/year/cert fields. Read by value into the Place event.
fn current_window_type(view: &SketchView<'_>) -> WindowTypeChoice {
    match *view.wt_existing_id {
        Some(id) if view.existing_window_types.iter().any(|t| t.id == id) => {
            WindowTypeChoice::Existing(id)
        }
        _ => WindowTypeChoice::New {
            glazing: view.wt_glazing.clone(),
            frame: view.wt_frame.clone(),
            year: view.wt_year_input.trim().parse().unwrap_or(0),
            certified: *view.wt_certified,
            draught_stripped: *view.wt_draught_stripped,
            gap: parse_gaps(view.wt_gap_input),
            name: {
                let n = view.wt_name_input.trim();
                (!n.is_empty()).then(|| n.to_string())
            },
        },
    }
}

/// Parse the gap field into mm values — comma/space/slash separated (triple
/// glazing has two gaps). Non-numeric and zero entries are dropped.
fn parse_gaps(input: &str) -> Vec<u32> {
    input
        .split([',', ' ', '/'])
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| n > 0)
        .collect()
}

fn glazing_label(g: &Glazing) -> String {
    match g {
        Glazing::Single => "Single".to_string(),
        Glazing::Double => "Double".to_string(),
        Glazing::Triple => "Triple".to_string(),
        Glazing::Other(s) => s.clone(),
    }
}

fn frame_label(f: &Frame) -> String {
    match f {
        Frame::WoodUPVC => "Wood/UPVC".to_string(),
        Frame::Metal => "Metal".to_string(),
        Frame::Other(s) => s.clone(),
    }
}

fn overshading_label(o: Overshading) -> &'static str {
    match o {
        Overshading::Heavy => "Heavy",
        Overshading::MoreThanAverage => "More than average",
        Overshading::Average => "Average",
        Overshading::VeryLittle => "Very little",
    }
}

/// Per-window overshading dropdown for the placement box (defaults to Average).
fn overshading_picker(ui: &mut egui::Ui, overshading: &mut Overshading) {
    ui.horizontal(|ui| {
        ui.label("overshading");
        egui::ComboBox::from_id_salt("opening_overshading")
            .selected_text(overshading_label(*overshading))
            .show_ui(ui, |ui| {
                for o in [
                    Overshading::Heavy,
                    Overshading::MoreThanAverage,
                    Overshading::Average,
                    Overshading::VeryLittle,
                ] {
                    ui.selectable_value(overshading, o, overshading_label(o));
                }
            });
    });
}

/// The door-kind dropdown for the "Edit door" box (`AssemblyPanel` is not
/// offered — that kind is owned by the door+sidelights assembly flow).
fn door_kind_picker(ui: &mut egui::Ui, kind: &mut DoorKind) {
    let label = |k: &DoorKind| match k {
        DoorKind::Solid => "Solid",
        DoorKind::HalfGlazed => "Half-glazed",
        DoorKind::FullyGlazed => "Fully glazed",
        DoorKind::AssemblyPanel => "Assembly panel",
    };
    ui.horizontal(|ui| {
        ui.label("kind");
        egui::ComboBox::from_id_salt("door_edit_kind")
            .selected_text(label(kind))
            .show_ui(ui, |ui| {
                for k in [DoorKind::Solid, DoorKind::HalfGlazed, DoorKind::FullyGlazed] {
                    let t = label(&k);
                    ui.selectable_value(kind, k, t);
                }
            });
    });
}

/// Photo control for a window-type group in the placement box: thumbnails of the
/// photos picked so far plus an "add photo" button (file dialog on desktop,
/// camera on Android). The paths are attached to the window type on Place. At
/// least one is required for a NEW group (`required`); adding to an existing
/// group is optional.
fn window_photo_picker(
    ui: &mut egui::Ui,
    pending: &mut Vec<(std::path::PathBuf, PhotoQuality)>,
    required: bool,
) {
    ui.horizontal(|ui| {
        ui.label("photo");
        for (p, _quality) in pending.iter() {
            let src = p.to_string_lossy();
            ui.add(
                egui::Image::new(format!("file://{src}"))
                    .fit_to_exact_size(egui::vec2(40.0, 40.0))
                    .maintain_aspect_ratio(true)
                    .corner_radius(3.0),
            );
        }
        // TEMPORARY dual hi/lo capture (see `photo_button_pair`).
        if let Some(captured) =
            photo_button_pair(ui, "📷 Add photo", true, PhotoQuality::High, false)
        {
            pending.push(captured);
        }
    });
    if required && pending.is_empty() {
        ui.label(RichText::new("⚠ add at least one photo").color(Color32::from_rgb(255, 120, 90)));
    }
}

/// Render the window-type picker for the placement box: a dropdown of the types
/// already in use (pick one to JOIN it by id) plus "New type…", and — when New —
/// the glazing/frame/year/cert fields. `ensure_window_type` dedups on Place, so a
/// New type that matches an existing one joins it ("only add if different").
#[allow(clippy::too_many_arguments)]
fn window_type_picker(
    ui: &mut egui::Ui,
    existing: &[ExistingWindowType],
    existing_id: &mut Option<ItemId>,
    glazing: &mut Glazing,
    frame: &mut Frame,
    certified: &mut bool,
    draught_stripped: &mut bool,
    name: &mut String,
) {
    ui.separator();
    ui.label(RichText::new("Window type").color(Color32::from_gray(170)));
    // Drop a stale selection (its type was since deleted) so the combo reads New.
    let stale = match existing_id {
        Some(id) => !existing.iter().any(|t| t.id == *id),
        None => false,
    };
    if stale {
        *existing_id = None;
    }
    let selected_text = match existing_id {
        Some(id) => existing
            .iter()
            .find(|t| t.id == *id)
            .map(|t| t.label.clone())
            .unwrap_or_else(|| "New type…".to_string()),
        None => "New type…".to_string(),
    };
    egui::ComboBox::from_id_salt("opening_window_type")
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            for t in existing {
                ui.selectable_value(existing_id, Some(t.id), &t.label);
            }
            ui.selectable_value(existing_id, None, "New type…");
        });
    // New-type fields: shown only when defining a new type (not reusing one).
    if existing_id.is_none() {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("opening_wt_glazing")
                .selected_text(glazing_label(glazing))
                .show_ui(ui, |ui| {
                    ui.selectable_value(glazing, Glazing::Single, "Single");
                    ui.selectable_value(glazing, Glazing::Double, "Double");
                    ui.selectable_value(glazing, Glazing::Triple, "Triple");
                });
            egui::ComboBox::from_id_salt("opening_wt_frame")
                .selected_text(frame_label(frame))
                .show_ui(ui, |ui| {
                    ui.selectable_value(frame, Frame::WoodUPVC, "Wood/UPVC");
                    ui.selectable_value(frame, Frame::Metal, "Metal");
                });
        });
        // Year + gap are entered up top as box fields (driven by the box's own
        // keypad); only the certification toggle + the type NAME stay here.
        ui.horizontal(|ui| {
            ui.checkbox(certified, "certified");
            ui.checkbox(draught_stripped, "draught stripped");
        });
        ui.horizontal(|ui| {
            ui.label("name");
            text_input(ui, name, KEY * 3.0, "model (required)");
        });
        // The model name is required for a new type (it disambiguates two
        // otherwise-identical windows installed at once). Gates the Place button.
        if name.trim().is_empty() {
            ui.label(RichText::new("⚠ enter a model name").color(Color32::from_rgb(255, 120, 90)));
        }
    }
}

/// Single-value floating box — the common case (one unlabelled field). Thin
/// wrapper over [`show_box_multi`] so every prompt shares one renderer.
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
    mode: &mut InputMode,
    mode_pinned: &mut bool,
    laser: &mut LaserSource,
    // A stable per-prompt token when this box is PRE-FILLED with a suggested
    // value (so the first typed key replaces it — see `apply_over_suggestion`).
    // `None` for boxes with no suggestion (they're never pristine).
    suggestion_token: Option<&str>,
    on_event: &mut F,
) {
    let mut fields = [BoxField {
        label: "",
        hint,
        buf,
        required: true,
        enabled: true,
        measurable: true,
        check: None,
    }];
    show_box_multi(
        ctx,
        rect,
        title,
        subtitle,
        error,
        &mut fields,
        &mut [],
        actions,
        mode,
        mode_pinned,
        None,
        None,
        None,
        laser,
        &mut |_| {},
        true,
        suggestion_token,
        on_event,
    );
}

/// Draw the floating window: the input-mode toggle, the question (title bar /
/// drag handle), one or more labelled expression fields, the shared input area
/// (keypad or laser), then the action buttons. The keypad / laser act on the
/// FOCUSED field; a laser reading auto-advances focus to the next field, so the
/// assessor can shoot Width, then Height, without touching the screen.
#[allow(clippy::too_many_arguments)]
fn show_box_multi<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    rect: egui::Rect,
    title: &str,
    subtitle: Option<&str>,
    error: Option<&str>,
    fields: &mut [BoxField<'_>],
    checks: &mut [BoxCheck<'_>],
    actions: &[Action],
    mode: &mut InputMode,
    mode_pinned: &mut bool,
    // The Door + sidelights Overall/Detailed switch, drawn under the keypad/laser
    // toggle. `None` for every other prompt (no switch shown).
    ds_mode: Option<&mut DoorSidelightsMode>,
    // The Complex-window shape dropdown, drawn under the toggles. `None` for
    // every other prompt.
    cx_kind: Option<&mut ComplexWindowKind>,
    // The plain Window tool's Flat / Semi-circular / Pointed top tick, drawn
    // with the other toggles. `None` for every other prompt.
    win_top: Option<&mut WindowTopShape>,
    laser: &mut LaserSource,
    // Extra controls drawn in the box body, under the toggles and above the
    // keypad — e.g. the openings window-type picker. A no-op for most prompts.
    extra: &mut dyn FnMut(&mut egui::Ui),
    // Extra validity gate for actions marked `.gated()` (e.g. the openings
    // Place/Fill, disabled until the window-type name is entered). Always `true`
    // for prompts with no such requirement.
    extra_valid: bool,
    // A stable per-prompt token when THIS box is PRE-FILLED with a suggested
    // value; `None` for boxes with no suggestion. See the pristine handling
    // inside the window body.
    suggestion_token: Option<&str>,
    on_event: &mut F,
) {
    let focus_id = Id::new(FOCUS_MEM);
    let parse_all = |fields: &[BoxField<'_>]| -> Vec<Option<f32>> {
        fields
            .iter()
            .map(|f| parse_dimension_expression(f.buf.as_str()))
            .collect()
    };
    // While the native keyboard is up it claims the bottom of the SCREEN —
    // shrink the constrain rect so the box gets pushed above it (and settles
    // back when the keyboard hides). The inset is measured from the screen
    // bottom, so clamp against that, not the canvas bottom. Never below 40%
    // of the canvas height, so an oversized keyboard can't squash the
    // constraint into nothing.
    #[cfg(target_os = "android")]
    let rect = {
        let keyboard_top =
            ctx.content_rect().bottom() - crate::ui::widgets::native_ime::inset_bottom_points(ctx);
        let bottom = rect
            .max
            .y
            .min(keyboard_top)
            .max(rect.min.y + rect.height() * 0.4);
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, bottom))
    };
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
            mode_toggle(ui, mode, mode_pinned);
            if let Some(ds_mode) = ds_mode {
                door_sidelights_mode_toggle(ui, ds_mode);
            }
            if let Some(cx_kind) = cx_kind {
                complex_window_shape_dropdown(ui, cx_kind);
            }
            if let Some(top) = win_top {
                window_top_toggle(ui, top);
            }
            if let Some(sub) = subtitle {
                ui.label(RichText::new(sub).color(Color32::from_gray(170)));
            }

            // Suggestion "pristine" tracking (see `apply_over_suggestion`): a box
            // pre-filled with a suggested value marks itself pristine when its
            // prompt IDENTITY (the token) changes, so the FIRST content key
            // REPLACES the suggestion instead of appending. Boxes with no
            // suggestion pass `None` and are never pristine. Consumed on the first
            // keypad key, on a laser reading, or on any hardware-keyboard edit.
            let token = suggestion_token.unwrap_or("");
            let token_id = Id::new("ber_number_entry_suggestion_token");
            let pristine_id = Id::new("ber_number_entry_suggestion_pristine");
            let mut pristine = ui.ctx().data_mut(|d| {
                if d.get_temp::<String>(token_id).as_deref() != Some(token) {
                    d.insert_temp(token_id, token.to_string());
                    // Entering a suggestion prompt (non-empty token) starts
                    // pristine; leaving one clears it.
                    let p = !token.is_empty();
                    d.insert_temp(pristine_id, p);
                    p
                } else {
                    d.get_temp::<bool>(pristine_id).unwrap_or(false)
                }
            });

            // Which field the keypad / laser drives. Persisted across frames so a
            // button press doesn't lose it; updated when a field gains focus.
            let mut focus = ui
                .ctx()
                .data(|d| d.get_temp::<usize>(focus_id))
                .unwrap_or(0)
                .min(fields.len().saturating_sub(1));
            // Never drive a disabled field — snap to the next enabled one (the
            // first field is always enabled, so this terminates).
            if fields.get(focus).is_some_and(|f| !f.enabled) {
                focus = fields.iter().position(|f| f.enabled).unwrap_or(0);
            }

            // One row per field: optional label + an editable expression box
            // (so a hardware keyboard still works). Tapping a box focuses it.
            let mut responses = Vec::with_capacity(fields.len());
            let mut enter = false;
            // Set when the NATIVE keyboard edited a field this frame (Android):
            // real editing, so it clears `pristine` like a hardware edit does.
            #[allow(unused_mut)]
            let mut ime_edited = false;
            let labelled = fields.iter().any(|f| !f.label.is_empty());
            for (i, f) in fields.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    if labelled {
                        let col = if !f.enabled {
                            Color32::from_gray(90)
                        } else if i == focus {
                            Color32::WHITE
                        } else {
                            Color32::from_gray(150)
                        };
                        ui.add_sized(
                            [54.0, KEY * 0.5],
                            egui::Label::new(RichText::new(f.label).color(col)).selectable(false),
                        );
                    }
                    let resp = ui.add_enabled(
                        f.enabled,
                        egui::TextEdit::singleline(f.buf)
                            .desired_width(KEY * 4.0 + 18.0)
                            .font(egui::TextStyle::Heading)
                            .hint_text(f.hint),
                    );
                    // Tapping the field (rather than the box's keypad) raises
                    // the native numeric keyboard and mirrors it into the buf.
                    #[cfg(target_os = "android")]
                    if f.enabled {
                        use crate::ui::widgets::native_ime::{attach, ImeKind};
                        let ime = attach(ui, &resp, f.buf, ImeKind::Numeric);
                        ime_edited |= ime.changed;
                        enter |= ime.submitted;
                    }
                    if resp.gained_focus() {
                        focus = i;
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        enter = true;
                    }
                    // The field's inline toggle (e.g. "Full width"), beside it.
                    if let Some(c) = f.check.as_mut() {
                        ui.checkbox(c.value, c.label);
                    }
                    responses.push(resp);
                });
            }

            // A hardware-keyboard (or native soft-keyboard) edit of the field
            // is real editing — drop the pristine suggestion so a later keypad
            // tap appends rather than clearing (the assessor deliberately
            // edited the value).
            if ime_edited || responses.iter().any(|r| r.changed()) {
                pristine = false;
            }

            // Boolean toggles (e.g. "semicircular transom"), under the fields.
            for c in checks.iter_mut() {
                ui.checkbox(c.value, c.label);
            }

            // Prompt-specific extra controls (e.g. the window-type picker).
            extra(ui);

            ui.add_space(4.0);
            let mut fired: Option<usize> = None;
            let mut laser_advanced = false;
            ui.horizontal(|ui| {
                match mode {
                    InputMode::Keypad => {
                        // Stay attached for detection, but don't let readings
                        // pile up while the keypad's in charge.
                        let _ = laser.poll();
                        digit_grid(ui, &mut fields[focus].buf, &mut pristine);
                        plus_minus(ui, &mut fields[focus].buf, &mut pristine);
                    }
                    // Laser mode, but the focused field can't be measured (a
                    // year / gap): use the keypad for it, so a typed value never
                    // needs a second pad — the box's keypad IS the numeric input.
                    InputMode::Laser if !fields[focus].measurable => {
                        let _ = laser.poll();
                        digit_grid(ui, &mut fields[focus].buf, &mut pristine);
                        plus_minus(ui, &mut fields[focus].buf, &mut pristine);
                    }
                    InputMode::Laser => {
                        // A reading sets the focused field, then focus advances to
                        // the next one (shoot Width → Height → … hands-free). +/−
                        // still sum several shots into one field.
                        if let Some(reading) = laser.poll() {
                            crate::number_entry::set_current_term(
                                &mut fields[focus].buf,
                                reading.metres,
                            );
                            // The reading replaced the term — the suggestion is gone.
                            pristine = false;
                            laser_advanced = true;
                        }
                        laser_status_panel(ctx, ui, laser);
                        plus_minus(ui, &mut fields[focus].buf, &mut pristine);
                    }
                }
                ui.add_space(8.0);
                // Parse AFTER the input edits so a just-pressed key (or a fresh
                // laser reading) enables the action buttons this same frame.
                let parsed = parse_all(fields);
                let all_required = fields
                    .iter()
                    .zip(&parsed)
                    .all(|(f, v)| !f.required || v.is_some());
                ui.vertical(|ui| {
                    for (ai, a) in actions.iter().enumerate() {
                        let enabled = (!a.needs_value || all_required) && (!a.gated || extra_valid);
                        if action_button(ui, a, enabled) {
                            fired = Some(ai);
                        }
                    }
                });
            });

            if let Some(err) = error {
                ui.label(RichText::new(format!("⚠ {err}")).color(Color32::from_rgb(255, 120, 90)));
            }

            let parsed = parse_all(fields);
            let all_required = fields
                .iter()
                .zip(&parsed)
                .all(|(f, v)| !f.required || v.is_some());
            // Enter triggers the primary action when its value requirement is met.
            if enter && fired.is_none() {
                if let Some(ai) = actions.iter().position(|a| a.primary) {
                    if (!actions[ai].needs_value || all_required)
                        && (!actions[ai].gated || extra_valid)
                    {
                        fired = Some(ai);
                    }
                }
            }
            if let Some(ai) = fired {
                let a = &actions[ai];
                on_event(UiEvent::Sketch((a.event)(&parsed)));
                if a.clear {
                    for f in fields.iter_mut() {
                        f.buf.clear();
                    }
                    focus = 0;
                }
            } else if laser_advanced {
                // Move to the next ENABLED, MEASURABLE field for the next shot
                // (skip greyed-out sills and typed-only fields like year / gap).
                if let Some(next) =
                    (focus + 1..fields.len()).find(|&i| fields[i].enabled && fields[i].measurable)
                {
                    focus = next;
                    responses[focus].request_focus();
                    // Programmatic focus move — the assessor is shooting the
                    // laser, not typing. Don't pop the native keyboard over
                    // the box; a physical tap on the field still raises it.
                    #[cfg(target_os = "android")]
                    crate::ui::widgets::native_ime::suppress_next_show();
                }
            }
            ui.ctx().data_mut(|d| d.insert_temp(focus_id, focus));
            ui.ctx().data_mut(|d| d.insert_temp(pristine_id, pristine));
        });
}

/// The 3-wide digit grid (`7-8-9` / `4-5-6` / `1-2-3` / `0 . Del`). Each press
/// is just a [`Key`] handed to the input engine — this fn owns only the layout,
/// not what a key means.
fn digit_grid(ui: &mut egui::Ui, buf: &mut String, pristine: &mut bool) {
    ui.vertical(|ui| {
        for row in [['7', '8', '9'], ['4', '5', '6'], ['1', '2', '3']] {
            ui.horizontal(|ui| {
                for d in row {
                    if digit(ui, &d.to_string()) {
                        apply_over_suggestion(buf, Key::Digit(d), pristine);
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            if digit(ui, "0") {
                apply_over_suggestion(buf, Key::Digit('0'), pristine);
            }
            if digit(ui, ".") {
                apply_over_suggestion(buf, Key::Dot, pristine);
            }
            if digit(ui, "Del") {
                apply_over_suggestion(buf, Key::Backspace, pristine);
            }
        });
    });
}

/// The operator column beside the digits: **+** on the top half, **−** on the
/// bottom half, each spanning two key rows. `+` adds the next length to the
/// running total; `−` subtracts it.
fn plus_minus(ui: &mut egui::Ui, buf: &mut String, pristine: &mut bool) {
    let half = egui::vec2(KEY * 1.5, KEY * 2.0);
    ui.vertical(|ui| {
        if ui
            .add(op_button("+", Color32::from_rgb(46, 110, 70), half))
            .hint("Add another length to the running total")
            .clicked()
        {
            apply_over_suggestion(buf, Key::Plus, pristine);
        }
        if ui
            .add(op_button("−", Color32::from_rgb(150, 80, 50), half))
            .hint("Subtract the next length from the running total")
            .clicked()
        {
            apply_over_suggestion(buf, Key::Minus, pristine);
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

/// The input-mode switch at the top of the box: keypad vs Bluetooth laser.
/// Any click pins the choice, so auto-detection won't override it afterwards.
fn mode_toggle(ui: &mut egui::Ui, mode: &mut InputMode, pinned: &mut bool) {
    ui.horizontal(|ui| {
        if ui
            .selectable_label(*mode == InputMode::Keypad, "🔢 Keypad")
            .hint("Type the measurement on the keypad")
            .clicked()
        {
            *mode = InputMode::Keypad;
            *pinned = true;
        }
        if ui
            .selectable_label(*mode == InputMode::Laser, "📡 Laser")
            .hint("Read the measurement from a paired Leica DISTO laser meter")
            .clicked()
        {
            *mode = InputMode::Laser;
            *pinned = true;
        }
    });
    ui.separator();
}

/// The Door + sidelights input-mode switch, under the keypad/laser toggle:
/// measure the whole doorset first (Overall) or type each piece (Detailed). The
/// choice persists on `CategoryUiState`, so it sticks across placements.
fn door_sidelights_mode_toggle(ui: &mut egui::Ui, mode: &mut DoorSidelightsMode) {
    ui.horizontal(|ui| {
        if ui
            .selectable_label(*mode == DoorSidelightsMode::Overall, "◱ Whole doorset")
            .hint("Measure the whole doorset, then the door within it (4 shots)")
            .clicked()
        {
            *mode = DoorSidelightsMode::Overall;
        }
        if ui
            .selectable_label(*mode == DoorSidelightsMode::Detailed, "▦ Piece by piece")
            .hint("Type each piece: door, then each sidelight + sill, then transom")
            .clicked()
        {
            *mode = DoorSidelightsMode::Detailed;
        }
    });
    ui.separator();
}

/// The plain Window tool's shaped-top 3-way tick, under the input-mode toggle:
/// Flat / Semi-circular top / Pointed top. Ticking fully determines the shape
/// (semi-circle radius = width/2, pointed apex triangle height = width/2 — no
/// extra dimensions; the entered height is the TOTAL at the centre). The
/// choice persists on `CategoryUiState`, so it sticks across placements.
fn window_top_toggle(ui: &mut egui::Ui, top: &mut WindowTopShape) {
    ui.horizontal(|ui| {
        for t in WindowTopShape::ALL {
            let hint = match t {
                WindowTopShape::Flat => "A plain rectangular window",
                WindowTopShape::SemiCircular => {
                    "Cap the window with a semicircle (radius = half the width); height is the total at the centre"
                }
                WindowTopShape::Pointed => {
                    "Cap the window with a centred point (half the width tall); height is the total at the apex"
                }
            };
            if ui.selectable_label(*top == t, t.label()).hint(hint).clicked() {
                *top = t;
            }
        }
    });
    ui.separator();
}

/// The Complex-window shape dropdown, under the input-mode toggle. Picking a
/// shape reshapes the box's fields on the next frame. The choice persists on
/// `CategoryUiState`, so it sticks across placements.
fn complex_window_shape_dropdown(ui: &mut egui::Ui, kind: &mut ComplexWindowKind) {
    ui.horizontal(|ui| {
        ui.label("Shape");
        egui::ComboBox::from_id_salt("complex_window_shape")
            .selected_text(kind.label())
            .show_ui(ui, |ui| {
                for k in ComplexWindowKind::ALL {
                    ui.selectable_value(kind, k, k.label());
                }
            });
    });
    ui.separator();
}

/// Keep the laser attached in the background whenever the Sketch page is up, and
/// — until the assessor pins a mode by hand — auto-switch into Laser mode the
/// moment a connected DISTO is detected. Runs every frame from [`render`].
fn laser_autodetect(ctx: &egui::Context, view: &mut SketchView<'_>) {
    let status = view.laser.status();
    if matches!(status, Status::Unsupported) {
        return;
    }
    // Start (or restart) the attach whenever we're idle — once; on Error we
    // wait for a manual Retry rather than retry-storming.
    if matches!(status, Status::Idle) {
        view.laser.connect();
    }
    // First detection auto-selects Laser, unless the assessor already chose.
    if matches!(view.laser.status(), Status::Connected)
        && !view.cat_state.input_mode_pinned
        && view.cat_state.input_mode == InputMode::Keypad
    {
        view.cat_state.input_mode = InputMode::Laser;
    }
    // Repaint while attaching/connected so the transition + readings are seen
    // even in Keypad mode (where the laser panel isn't driving repaints).
    if view.laser.status().is_active() {
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

/// Laser-mode status panel (replaces the keypad): pure DISPLAY. There's no
/// connect button and no scan — entering Laser mode auto-attaches to the
/// already-paired Disto and listens. The assessor points the device and presses
/// ITS button; the reading is polled + applied by the caller ([`show_box_multi`],
/// which then advances focus). Shows a status line, and a Retry only if attaching
/// failed. Sized to roughly match the keypad's footprint so the box doesn't jump.
fn laser_status_panel(ctx: &egui::Context, ui: &mut egui::Ui, laser: &mut LaserSource) {
    // Attaching is owned by `laser_autodetect` (runs every frame); here we just
    // reflect status and offer a manual Retry on error.
    let status = laser.status();

    ui.vertical(|ui| {
        ui.set_min_width(KEY * 3.5);
        let (icon, text, colour) = match &status {
            Status::Unsupported => (
                "🚫",
                "Bluetooth isn't built into this app".to_string(),
                Color32::from_gray(160),
            ),
            // Idle is transient here (we just kicked off connect), show it as
            // starting up rather than "off".
            Status::Idle | Status::Connecting => (
                "📡",
                "Connecting to the laser…".to_string(),
                Color32::from_rgb(230, 190, 70),
            ),
            Status::Connected => (
                "🟢",
                "Listening — press the button on the laser".to_string(),
                Color32::from_rgb(90, 190, 110),
            ),
            Status::Error(e) => ("⚠", e.clone(), Color32::from_rgb(235, 110, 90)),
        };
        ui.label(RichText::new(format!("{icon}  {text}")).color(colour));
        ui.add_space(4.0);
        ui.label(
            RichText::new("Point the Disto, then press its measure button.")
                .size(11.0)
                .weak(),
        );
        if matches!(status, Status::Error(_)) {
            ui.add_space(4.0);
            if ui
                .add(
                    egui::Button::new(RichText::new("Retry").color(Color32::WHITE))
                        .min_size(egui::vec2(KEY * 2.0, KEY))
                        .fill(Color32::from_rgb(48, 96, 158)),
                )
                .clicked()
            {
                laser.connect();
            }
        }
    });

    // Keep repainting while in Laser mode so the connect transition advances and
    // freshly-arrived readings are polled without other UI input.
    ctx.request_repaint_after(Duration::from_millis(150));
}
