//! Touch-friendly text entry: the shared entry fields.
//!
//! [`metre_input`] is the metre-value field used by every measurement prompt
//! (dimension, position, heights, roof inputs, …); [`text_input`] is the
//! free-text equivalent (names, nicknames).
//!
//! - **Android**: focusing the field raises the NATIVE soft keyboard (numeric
//!   or text layout per field) via `native_ime` — GameActivity's GameTextInput
//!   path. The on-screen pads below are not rendered.
//! - **Desktop with `BER_TOUCH=1`**: focusing the field opens a shared
//!   on-screen pad (numeric or qwerty) that edits the field's backing `String`
//!   directly. This is the pads' development path and the fallback should the
//!   native keyboard regress — don't delete them.
//! - **Plain desktop**: both are exactly the `TextEdit` idiom they replaced.
//!
//! One pad exists at a time: ownership (which field the pad edits) is keyed
//! by the `TextEdit`'s widget id in egui memory, claimed on focus, released
//! by OK (submit), Hide (dismiss), or another field claiming it. The pad lives
//! in a floating `Area`, so widget state stays inside `src/ui/widgets` —
//! no per-page plumbing.

use egui::{Align2, Id, RichText};

/// Result of one input frame.
pub struct PadResponse {
    /// User submitted: Enter (hardware keyboard) or the pad's ✓.
    pub submitted: bool,
    /// Buffer changed this frame (typed or via the pad).
    pub changed: bool,
}

/// Are the on-screen pads active? Desktop opts in with `BER_TOUCH=1` (any
/// value but `0`) so the pads can be developed and visually tested without a
/// tablet. Android no longer uses them — fields raise the native soft
/// keyboard instead (`native_ime`).
fn pads_enabled() -> bool {
    if cfg!(target_os = "android") {
        return false;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("BER_TOUCH").is_ok_and(|v| v != "0"))
}

fn owner_key() -> Id {
    Id::new("ber_touch_pad_owner")
}

fn shift_key() -> Id {
    Id::new("ber_touch_pad_shift")
}

/// What a pad interaction did to the owning field this frame.
enum PadEvent {
    None,
    Edited,
    Submit,
    Dismiss,
}

/// The metre-value field: a singleline `TextEdit` (width/hint preserved from
/// the call sites) that opens the shared NUMERIC pad on focus when pads are
/// enabled. Digits, `.`, `+` (dimension expressions accept sums), Del, OK.
pub fn metre_input(ui: &mut egui::Ui, buf: &mut String, width: f32, hint: &str) -> PadResponse {
    input_with_pad(ui, buf, width, hint, PadKind::Numeric)
}

/// The free-text field: the native text keyboard on Android; the shared
/// QWERTY pad on desktop when pads are enabled (`BER_TOUCH=1`).
pub fn text_input(ui: &mut egui::Ui, buf: &mut String, width: f32, hint: &str) -> PadResponse {
    input_with_pad(ui, buf, width, hint, PadKind::Qwerty)
}

/// The MULTILINE free-text field (notes, the follow-up email body): a
/// multiline `TextEdit` that raises the native text keyboard on Android with
/// a REAL Enter — newlines commit into the buffer instead of acting as
/// "Done" (`ImeKind::TextMultiline`) — and opens the shared QWERTY pad on
/// desktop with `BER_TOUCH=1` (the pad has no newline key; it's the dev
/// fallback). There is no Enter-submit idiom: `submitted` fires only from
/// the pad's OK.
pub fn text_input_multiline(
    ui: &mut egui::Ui,
    buf: &mut String,
    rows: usize,
    hint: &str,
) -> PadResponse {
    let response = ui.add(
        egui::TextEdit::multiline(buf)
            .desired_width(f32::INFINITY)
            .desired_rows(rows)
            .hint_text(hint),
    );
    let mut submitted = false;
    let mut changed = response.changed();

    // Android: the field drives the native soft keyboard instead of the pads.
    #[cfg(target_os = "android")]
    {
        use super::native_ime::{attach, ImeKind};
        let ime = attach(ui, &response, buf, ImeKind::TextMultiline);
        changed |= ime.changed;
        submitted |= ime.submitted;
    }

    desktop_pad(
        ui,
        &response,
        buf,
        PadKind::Qwerty,
        &mut submitted,
        &mut changed,
    );

    PadResponse { submitted, changed }
}

/// [`text_input`] that SELECTS the whole current text when the field GAINS
/// focus, so typing replaces it without manual clearing — the group-name
/// field idiom ("Group 1" should vanish on the first keystroke).
pub fn text_input_select_all(
    ui: &mut egui::Ui,
    buf: &mut String,
    width: f32,
    hint: &str,
) -> PadResponse {
    input_with_pad_opts(ui, buf, width, hint, PadKind::Qwerty, true)
}

#[derive(Clone, Copy, PartialEq)]
enum PadKind {
    Numeric,
    Qwerty,
}

fn input_with_pad(
    ui: &mut egui::Ui,
    buf: &mut String,
    width: f32,
    hint: &str,
    kind: PadKind,
) -> PadResponse {
    input_with_pad_opts(ui, buf, width, hint, kind, false)
}

fn input_with_pad_opts(
    ui: &mut egui::Ui,
    buf: &mut String,
    width: f32,
    hint: &str,
    kind: PadKind,
    select_all_on_focus: bool,
) -> PadResponse {
    let char_len = buf.chars().count();
    let response = ui.add(
        egui::TextEdit::singleline(buf)
            .desired_width(width)
            .hint_text(hint),
    );
    // SELECT-ALL on gaining focus (the group-name idiom): set the stored
    // `TextEditState` cursor range over the whole text, so the next
    // keystroke replaces it.
    if select_all_on_focus && response.gained_focus() {
        use egui::text::{CCursor, CCursorRange};
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), response.id) {
            state.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(0),
                CCursor::new(char_len),
            )));
            state.store(ui.ctx(), response.id);
        }
    }
    let mut submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let mut changed = response.changed();

    // Android: the field drives the native soft keyboard instead of the pads.
    #[cfg(target_os = "android")]
    {
        use super::native_ime::{attach, ImeKind};
        let ime = attach(
            ui,
            &response,
            buf,
            match kind {
                PadKind::Numeric => ImeKind::Numeric,
                PadKind::Qwerty => ImeKind::Text,
            },
        );
        changed |= ime.changed;
        submitted |= ime.submitted;
    }

    desktop_pad(ui, &response, buf, kind, &mut submitted, &mut changed);

    PadResponse { submitted, changed }
}

/// The desktop (`BER_TOUCH=1`) on-screen pad half of a field: claim the pad on
/// focus, apply its presses to `buf`, release on submit/dismiss. No-op when
/// pads are disabled (Android / plain desktop).
fn desktop_pad(
    ui: &egui::Ui,
    response: &egui::Response,
    buf: &mut String,
    kind: PadKind,
    submitted: &mut bool,
    changed: &mut bool,
) {
    if !pads_enabled() {
        return;
    }
    let ctx = ui.ctx().clone();
    if response.has_focus() {
        ctx.data_mut(|d| d.insert_temp(owner_key(), response.id));
    }
    let owner: Option<Id> = ctx.data(|d| d.get_temp(owner_key()));
    if owner == Some(response.id) {
        match render_pad(&ctx, buf, kind) {
            PadEvent::Edited => *changed = true,
            // `remove_temp` needs `T: Default`, which `Id` isn't —
            // overwrite with the never-matching NULL id instead.
            PadEvent::Submit => {
                *submitted = true;
                ctx.data_mut(|d| d.insert_temp(owner_key(), Id::NULL));
            }
            PadEvent::Dismiss => {
                ctx.data_mut(|d| d.insert_temp(owner_key(), Id::NULL));
            }
            PadEvent::None => {}
        }
    }
    // A hardware-Enter submit closes the pad too.
    if *submitted {
        ctx.data_mut(|d| d.insert_temp(owner_key(), Id::NULL));
    }
}

/// Minimum finger-target size for pad keys.
const KEY_SIZE: f32 = 44.0;

fn key(ui: &mut egui::Ui, label: &str) -> bool {
    ui.add(
        egui::Button::new(RichText::new(label).size(18.0)).min_size(egui::vec2(KEY_SIZE, KEY_SIZE)),
    )
    .clicked()
}

/// Draw the shared pad (a floating window) and apply its presses to `buf`.
///
/// It's an `egui::Window` so it comes with a title-bar drag handle — the
/// assessor can move it aside if it covers a field or a button — and it
/// remembers its dragged spot (stable `id`), staying on-screen (`constrain_to`).
/// It defaults to the BOTTOM-CENTRE: most fields sit up the top of the form, so
/// the bottom keeps them and the pad both visible, and it's the natural
/// thumb-reach spot on a tablet.
fn render_pad(ctx: &egui::Context, buf: &mut String, kind: PadKind) -> PadEvent {
    let mut event = PadEvent::None;
    let screen = ctx.content_rect();
    egui::Window::new("Keyboard")
        // Stable id so the dragged position is remembered while the pad is up
        // (the title changes nothing; the pad is keyed by the owning field).
        .id(Id::new("ber_touch_pad"))
        .order(egui::Order::Foreground)
        .resizable(false)
        .collapsible(false)
        // Default parking spot: centred along the bottom edge, a hair above it.
        // `pivot(CENTER_BOTTOM)` makes `default_pos` the pad's own bottom-centre.
        .pivot(Align2::CENTER_BOTTOM)
        .default_pos(screen.center_bottom() - egui::vec2(0.0, 12.0))
        .constrain_to(screen)
        .show(ctx, |ui| {
            event = match kind {
                PadKind::Numeric => numeric_rows(ui, buf),
                PadKind::Qwerty => qwerty_rows(ctx, ui, buf),
            };
        });
    event
}

fn numeric_rows(ui: &mut egui::Ui, buf: &mut String) -> PadEvent {
    let mut event = PadEvent::None;
    for row in [
        ["7", "8", "9"],
        ["4", "5", "6"],
        ["1", "2", "3"],
        ["0", ".", "+"],
    ] {
        ui.horizontal(|ui| {
            for label in row {
                if key(ui, label) {
                    buf.push_str(label);
                    event = PadEvent::Edited;
                }
            }
        });
    }
    ui.horizontal(|ui| {
        if key(ui, "Del") {
            if buf.pop().is_some() {
                event = PadEvent::Edited;
            }
        }
        if key(ui, "OK") {
            event = PadEvent::Submit;
        }
        if key(ui, "Hide") {
            event = PadEvent::Dismiss;
        }
    });
    event
}

fn qwerty_rows(ctx: &egui::Context, ui: &mut egui::Ui, buf: &mut String) -> PadEvent {
    let mut event = PadEvent::None;
    let mut shift: bool = ctx.data(|d| d.get_temp(shift_key())).unwrap_or(false);
    let mut shift_used = false;

    for row in ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"] {
        ui.horizontal(|ui| {
            // Centre the shorter rows roughly under the longer ones.
            ui.add_space((10 - row.chars().count()) as f32 * KEY_SIZE * 0.35);
            for ch in row.chars() {
                let shown = if shift {
                    ch.to_ascii_uppercase().to_string()
                } else {
                    ch.to_string()
                };
                if key(ui, &shown) {
                    buf.push_str(&shown);
                    event = PadEvent::Edited;
                    shift_used = true;
                }
            }
        });
    }
    ui.horizontal(|ui| {
        // Single-shot shift (mobile convention): clears after one character.
        let shift_label = if shift { "Shift*" } else { "Shift" };
        if key(ui, shift_label) {
            shift = !shift;
            shift_used = false;
        }
        if ui
            .add(
                egui::Button::new(RichText::new("space").size(18.0))
                    .min_size(egui::vec2(KEY_SIZE * 4.0, KEY_SIZE)),
            )
            .clicked()
        {
            buf.push(' ');
            event = PadEvent::Edited;
        }
        if key(ui, "Del") {
            if buf.pop().is_some() {
                event = PadEvent::Edited;
            }
        }
        if key(ui, "OK") {
            event = PadEvent::Submit;
        }
        if key(ui, "Hide") {
            event = PadEvent::Dismiss;
        }
    });
    if shift_used && shift {
        shift = false;
    }
    ctx.data_mut(|d| d.insert_temp(shift_key(), shift));
    event
}
