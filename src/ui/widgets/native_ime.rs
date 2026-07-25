//! Native Android soft keyboard (GameActivity + GameTextInput), Android-only.
//!
//! winit 0.30's Android backend can *show* the IME (`show_soft_input`) but
//! drops the `TextEvent`s GameTextInput delivers — its input match has no arm
//! for them — so typed text never arrives as egui events. This module goes
//! around winit using the `AndroidApp` text-state API directly: when a field
//! gains focus its backing `String` is seeded into GameTextInput's buffer
//! (`set_text_input_state`), and while it keeps the keyboard the buffer is
//! mirrored back every frame (`text_input_state`). Because the IME edits the
//! full field text — not an empty scratch buffer — backspace over pre-existing
//! text, autocomplete and compose regions all behave.
//!
//! One field owns the keyboard at a time (same ownership idea as the desktop
//! pads in `pads.rs`). [`attach`] is called right after a `TextEdit` by every
//! participating field; [`end_frame`] runs once per frame from `BerApp::update`
//! and hides the keyboard when no field wants it. The keyboard's inset height
//! arrives from `MainActivity.onImeInsetsChanged` via [`nativeOnImeInsets`],
//! so floating windows can be repositioned above the keyboard
//! ([`inset_bottom_points`]). Scrolling covered fields clear of the keyboard
//! is NOT this module's job: the shared fields in `pads.rs` own it
//! (`scroll_clear_of_keyboard` — pure egui-side scrolling on one-shot
//! focus/inset edges, so it can't fight this module's owner/session logic).
//!
//! The CURSOR is mirrored both ways alongside the text: egui's `TextEdit`
//! keeps its cursor in memory (`TextEditState`, char indices) and the IME
//! state carries a selection (UTF-16 code units). Tapping mid-text or using
//! arrow keys pushes egui's cursor to the IME, so typing/backspace happen at
//! the visible cursor; IME edits push the resulting cursor back into egui.
//! A selected range works too — the next keystroke replaces it, like a real
//! `EditText`.
//!
//! "Done"/Enter needs no wiring here: GameTextInput's `InputConnection` is a
//! `BaseInputConnection`, whose `performEditorAction` synthesizes
//! KEYCODE_ENTER — that reaches egui as a real Enter key through winit, so the
//! usual `lost_focus() && key_pressed(Enter)` submit idiom keeps working. As a
//! fallback for IMEs that commit a literal `'\n'` instead, the mirror strips
//! it and reports `submitted`.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use winit::platform::android::activity::input::{TextInputState, TextSpan};
use winit::platform::android::activity::AndroidApp;

/// Which system keyboard a field wants.
#[derive(Clone, Copy, PartialEq)]
pub enum ImeKind {
    /// Digits + decimal point (+ sign) — dimension/metre/year fields.
    Numeric,
    /// The full text keyboard — names, nicknames, notes.
    Text,
    /// The full text keyboard for a MULTILINE field (the follow-up email
    /// body): Enter commits a real newline instead of acting as "Done", so
    /// the mirror keeps `'\n'`s instead of stripping them.
    TextMultiline,
}

/// What the IME did to the attached field this frame.
#[derive(Default)]
pub struct ImeResponse {
    /// The mirror wrote a new value into the field's buffer.
    pub changed: bool,
    /// The IME committed a newline (some keyboards' "Done") — treat as submit.
    pub submitted: bool,
}

/// The `AndroidApp` handle, stashed by [`init`] before eframe starts. Widgets
/// are called deep in the render tree with no path to the app shell, so this
/// is a module global (one activity per process; the handle is cheap to clone
/// and thread-safe).
static APP: OnceLock<AndroidApp> = OnceLock::new();

/// IME inset (keyboard height) in physical pixels, pushed from the UI thread
/// by `MainActivity.onImeInsetsChanged` → [`nativeOnImeInsets`].
static IME_INSET_BOTTOM_PX: AtomicI32 = AtomicI32::new(0);

/// The keyboard session: which field owns it and the last text + selection we
/// exchanged with the IME (so the mirror only reacts to real changes, and can
/// tell WHICH side moved).
#[derive(Default)]
struct Session {
    owner: Option<egui::Id>,
    last_text: String,
    /// Selection last exchanged with the IME (UTF-16 code units, min ≤ max).
    last_sel: (usize, usize),
    /// egui cursor last seen/written for the owner (char indices, min ≤ max) —
    /// a divergence from this with a quiet IME means the assessor moved the
    /// cursor on the egui side (tap / arrows).
    last_egui_cursor: Option<(usize, usize)>,
    shown: bool,
    /// The keyboard layout the IME last bound. `setImeEditorInfoFields` is
    /// only read when an input session (re)starts, so a numeric↔text focus
    /// move while the keyboard stays up needs an explicit input restart.
    last_kind: Option<ImeKind>,
    /// One-shot: the next PROGRAMMATIC focus gain must not raise the keyboard
    /// (set via [`suppress_next_show`] by e.g. the laser auto-advance). A real
    /// tap on the field overrides it — `clicked()` always shows.
    suppress_next_show: bool,
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    owner: None,
    last_text: String::new(),
    last_sel: (0, 0),
    last_egui_cursor: None,
    shown: false,
    last_kind: None,
    suppress_next_show: false,
});

/// Store the `AndroidApp` handle. Called once from `ui::run_android` before
/// eframe takes over.
pub fn init(app: &AndroidApp) {
    let _ = APP.set(app.clone());
}

/// The keyboard height in egui points (0 when hidden).
pub fn inset_bottom_points(ctx: &egui::Context) -> f32 {
    IME_INSET_BOTTOM_PX.load(Ordering::Relaxed) as f32 / ctx.pixels_per_point()
}

/// Call right after a PROGRAMMATIC `request_focus()` (e.g. the laser
/// auto-advance moving to the next measurement field): the next [`attach`]
/// focus gain adopts the field WITHOUT raising the keyboard — the assessor is
/// shooting the laser, not typing. One-shot; consumed by the next focus gain.
/// A physical tap still shows the keyboard (a tap's `clicked()` overrides the
/// flag, and a tap on the already-focused field re-shows it as before), and if
/// the keyboard is already up it simply stays up and follows the new field.
pub fn suppress_next_show() {
    SESSION.lock().unwrap().suppress_next_show = true;
}

/// Attach the native keyboard to a `TextEdit`: call right after `ui.add(...)`
/// with the response and the field's backing buffer. Shows + seeds the IME on
/// focus gain and mirrors typed text back into `buf` while focused. (Keeping
/// the field visible above the keyboard is the caller's concern — the shared
/// fields in `pads.rs` do it via `scroll_clear_of_keyboard`.)
pub fn attach(
    ui: &mut egui::Ui,
    response: &egui::Response,
    buf: &mut String,
    kind: ImeKind,
) -> ImeResponse {
    let mut out = ImeResponse::default();
    let Some(app) = APP.get() else { return out };
    let mut s = SESSION.lock().unwrap();

    let focused = response.has_focus();
    let is_owner = s.owner == Some(response.id);

    // One-shot from `suppress_next_show`: consumed by whichever field gains
    // focus next (only that field reaches the gain branch below). A stale
    // flag can't eat a real tap — `clicked()` overrides it.
    let suppress = focused && !is_owner && std::mem::take(&mut s.suppress_next_show);

    if focused && !is_owner {
        // Field takes the keyboard: keyboard type first (it's read when the
        // IME binds), then seed the IME buffer with the field's current text
        // and the cursor egui placed from the tap — so the first key already
        // lands where the assessor pointed. When focus hops directly between
        // fields of DIFFERENT kinds the keyboard never goes down, so the IME
        // must be told to restart its input session — a bare show re-uses the
        // bound layout.
        s.owner = Some(response.id);
        if s.last_kind != Some(kind) {
            set_editor_info(kind);
            // Once any field has bound the IME, a session with the OLD layout
            // exists (it survives even a hide) — cycle it so the keyboard
            // re-reads the editor info. Skipped only on the very first bind.
            if s.last_kind.is_some() {
                restart_ime_input();
            }
            s.last_kind = Some(kind);
        }
        let cur = egui_cursor(ui.ctx(), response.id, buf);
        let sel = chars_to_utf16_sel(buf, cur);
        s.last_text = buf.clone();
        s.last_sel = sel;
        s.last_egui_cursor = Some(cur);
        push_state(app, buf, sel);
        // Programmatic focus (laser auto-advance) adopts the field without
        // raising the keyboard: if it's already up it stays up and follows
        // (`s.shown` unchanged); if it's down it stays down until a real tap.
        // A tap that lands focus this same frame shows as normal.
        if !suppress || s.shown || response.clicked() {
            app.show_soft_input(false);
            s.shown = true;
        } else {
            scrub_ime_output(ui.ctx());
        }
    } else if focused && is_owner {
        // The system back gesture can hide the keyboard without the app ever
        // learning — a tap on the still-focused field asks for it again. Also
        // the path that RAISES the keyboard on a field adopted silently by a
        // suppressed (laser auto-advance) focus move.
        if response.clicked() {
            app.show_soft_input(false);
            s.shown = true;
        }
        if !s.shown {
            // Still adopted-without-keyboard: keep the implicit show path off
            // (the focused `TextEdit` re-sets the IME output every frame).
            scrub_ime_output(ui.ctx());
        }
        if response.changed() || *buf != s.last_text {
            // The field's buffer moved on the app side this frame — an egui
            // edit (paste), a laser reading, or an action clearing the field.
            // Push it (with egui's cursor) so the IME doesn't revert it on
            // the next key.
            let cur = egui_cursor(ui.ctx(), response.id, buf);
            let sel = chars_to_utf16_sel(buf, cur);
            s.last_text = buf.clone();
            s.last_sel = sel;
            s.last_egui_cursor = Some(cur);
            push_state(app, buf, sel);
        } else {
            let state = app.text_input_state();
            let ime_sel = clamp_sel(&state.text, (state.selection.start, state.selection.end));
            if state.text != s.last_text {
                // The IME edited the text: mirror it AND the resulting cursor
                // into egui, so the visible caret tracks the edit point.
                let mut text = state.text;
                let mut sel = ime_sel;
                if text.contains('\n') && kind != ImeKind::TextMultiline {
                    // Newline commit = this keyboard's "Done". Strip it (the
                    // single-line fields; a multiline field keeps its real
                    // newlines) and re-push so the IME buffer matches what
                    // the field shows.
                    text.retain(|c| c != '\n');
                    out.submitted = true;
                    sel = clamp_sel(&text, sel);
                    push_state(app, &text, sel);
                }
                let cur = utf16_to_chars_sel(&text, sel);
                s.last_text = text.clone();
                s.last_sel = sel;
                if *buf != text {
                    *buf = text;
                    out.changed = true;
                }
                set_egui_cursor(ui.ctx(), response.id, cur);
                s.last_egui_cursor = Some(cur);
            } else if ime_sel != s.last_sel {
                // The IME moved its cursor without changing text (e.g. a
                // spacebar cursor-drag on some keyboards): follow in egui.
                s.last_sel = ime_sel;
                let cur = utf16_to_chars_sel(&state.text, ime_sel);
                set_egui_cursor(ui.ctx(), response.id, cur);
                s.last_egui_cursor = Some(cur);
            } else {
                // Both quiet: did the assessor move the EGUI cursor (tap in
                // the field, arrow keys, selection drag)? Push it to the IME
                // so the next key lands there. Ends any active compose region
                // — same as moving the cursor in a real EditText.
                let cur = egui_cursor(ui.ctx(), response.id, buf);
                if s.last_egui_cursor != Some(cur) {
                    s.last_egui_cursor = Some(cur);
                    let sel = chars_to_utf16_sel(buf, cur);
                    s.last_sel = sel;
                    push_state(app, buf, sel);
                }
            }
        }
    } else if !focused && is_owner {
        // Focus left this field. Release ownership; end_frame hides the
        // keyboard unless another field claimed it this frame.
        s.owner = None;
    }

    out
}

/// Once-per-frame keeper, called from `BerApp::update`: hides the keyboard
/// when no text field has focus any more (including when the owning widget
/// stopped being rendered, e.g. a prompt closed or the page changed), and
/// keeps frames coming while the keyboard is up — the mirror is a poll, and
/// winit forwards no event for IME text, so without this egui would idle and
/// typed text would only appear on the next touch.
pub fn end_frame(ctx: &egui::Context) {
    let Some(app) = APP.get() else { return };
    let mut s = SESSION.lock().unwrap();
    if !ctx.wants_keyboard_input() {
        s.owner = None;
    }
    if s.owner.is_none() && s.shown {
        app.hide_soft_input(false);
        s.shown = false;
    }
    if s.shown {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

/// Kill the SECOND, implicit keyboard-show path for this frame. This module's
/// explicit `show_soft_input` is not the only one: a focused `TextEdit` sets
/// `PlatformOutput::ime` every frame (egui `builder.rs`, guarded by
/// `has_focus`), egui-winit turns the None→Some flip into
/// `Window::set_ime_allowed(true)`, and winit's Android backend implements
/// that as `show_soft_input(true)`. Clearing the output here suppresses that
/// path with no flicker: [`attach`] runs directly after the owning `TextEdit`
/// (the only focused widget, hence the only `ime` writer), and egui-winit
/// reads the output only after the frame ends. Called every frame the owner
/// holds the session WITHOUT the keyboard (a suppressed laser-advance
/// adoption); once a tap sets `shown`, the output flows again and egui-winit's
/// show fires alongside ours — same as an ordinary tap-focus today.
fn scrub_ime_output(ctx: &egui::Context) {
    ctx.output_mut(|o| o.ime = None);
}

/// Seed/replace the IME's text buffer and selection. Spans are Java string
/// (UTF-16 code unit) indices — never byte or char indices.
fn push_state(app: &AndroidApp, text: &str, sel: (usize, usize)) {
    app.set_text_input_state(TextInputState {
        text: text.to_owned(),
        selection: TextSpan {
            start: sel.0,
            end: sel.1,
        },
        compose_region: None,
    });
}

/// The field's egui cursor as (min, max) CHAR indices, clamped to `text`;
/// falls back to end-of-text when the `TextEdit` has no stored cursor yet.
fn egui_cursor(ctx: &egui::Context, id: egui::Id, text: &str) -> (usize, usize) {
    let chars = text.chars().count();
    let range =
        egui::widgets::text_edit::TextEditState::load(ctx, id).and_then(|s| s.cursor.char_range());
    match range {
        Some(r) => {
            let (a, b) = (r.primary.index.min(chars), r.secondary.index.min(chars));
            (a.min(b), a.max(b))
        }
        None => (chars, chars),
    }
}

/// Write a cursor/selection (char indices) into the field's egui state — the
/// `TextEdit` picks it up on its next frame (and clamps it itself).
fn set_egui_cursor(ctx: &egui::Context, id: egui::Id, cur: (usize, usize)) {
    let mut state = egui::widgets::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(cur.0),
            egui::text::CCursor::new(cur.1),
        )));
    state.store(ctx, id);
}

/// Clamp an IME selection to `text` and order it (min, max). The IME reports
/// UTF-16 indices asynchronously, so a stale span can momentarily overshoot.
fn clamp_sel(text: &str, sel: (usize, usize)) -> (usize, usize) {
    let max = text.encode_utf16().count();
    let (a, b) = (sel.0.min(max), sel.1.min(max));
    (a.min(b), a.max(b))
}

/// Char-index selection → UTF-16 code-unit selection.
fn chars_to_utf16_sel(text: &str, cur: (usize, usize)) -> (usize, usize) {
    let at = |char_idx: usize| -> usize {
        text.chars()
            .take(char_idx)
            .map(|c| c.len_utf16())
            .sum::<usize>()
    };
    (at(cur.0), at(cur.1))
}

/// UTF-16 code-unit selection → char-index selection.
fn utf16_to_chars_sel(text: &str, sel: (usize, usize)) -> (usize, usize) {
    let at = |utf16_idx: usize| -> usize {
        let mut units = 0;
        for (chars, c) in text.chars().enumerate() {
            if units >= utf16_idx {
                return chars;
            }
            units += c.len_utf16();
        }
        text.chars().count()
    };
    (at(sel.0), at(sel.1))
}

// EditorInfo constants (android.text.InputType / android.view.inputmethod.EditorInfo).
const TYPE_CLASS_TEXT: i32 = 0x1;
const TYPE_CLASS_NUMBER: i32 = 0x2;
const TYPE_NUMBER_FLAG_SIGNED: i32 = 0x1000;
const TYPE_NUMBER_FLAG_DECIMAL: i32 = 0x2000;
const TYPE_TEXT_FLAG_MULTI_LINE: i32 = 0x2_0000;
const IME_ACTION_NONE: i32 = 0x1;
const IME_ACTION_DONE: i32 = 0x6;
/// Never take over the whole screen in landscape (the tablet's natural
/// orientation) — keep the app visible behind the keyboard.
const IME_FLAG_NO_EXTRACT_UI: i32 = 0x1000_0000;
const IME_FLAG_NO_FULLSCREEN: i32 = 0x0200_0000;

/// Tell GameActivity which keyboard the next show should bind
/// (`GameActivity.setImeEditorInfoFields` is a public Java method — the
/// C-side equivalent isn't exposed through android-activity's Rust API).
/// Same JNI idiom as the camera bridge in `photo.rs`.
fn set_editor_info(kind: ImeKind) {
    let input_type = match kind {
        ImeKind::Numeric => TYPE_CLASS_NUMBER | TYPE_NUMBER_FLAG_SIGNED | TYPE_NUMBER_FLAG_DECIMAL,
        ImeKind::Text => TYPE_CLASS_TEXT,
        // MULTI_LINE turns the action key into a real Enter (newline commit).
        ImeKind::TextMultiline => TYPE_CLASS_TEXT | TYPE_TEXT_FLAG_MULTI_LINE,
    };
    let action = match kind {
        ImeKind::TextMultiline => IME_ACTION_NONE,
        _ => IME_ACTION_DONE,
    };
    let ime_options = action | IME_FLAG_NO_EXTRACT_UI | IME_FLAG_NO_FULLSCREEN;
    if let Err(e) = call_set_editor_info(input_type, action, ime_options) {
        // Non-fatal: the default keyboard still types; only the layout hint
        // is lost.
        log::warn!("ime: setImeEditorInfoFields failed: {e}");
    }
}

fn call_set_editor_info(
    input_type: i32,
    action_id: i32,
    ime_options: i32,
) -> Result<(), jni::errors::Error> {
    use jni::objects::{JObject, JValue};
    // SAFETY: android-activity registers the JavaVM + activity with
    // `ndk_context` before `android_main` runs (so before any UI frame).
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }?;
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    env.call_method(
        &activity,
        "setImeEditorInfoFields",
        "(III)V",
        &[
            JValue::Int(input_type),
            JValue::Int(action_id),
            JValue::Int(ime_options),
        ],
    )?;
    Ok(())
}

/// `MainActivity.restartImeInput()` — restart the IME's input session so an
/// already-showing keyboard re-reads the editor info (only a session start
/// consumes it) and switches layout.
fn restart_ime_input() {
    fn call() -> Result<(), jni::errors::Error> {
        use jni::objects::JObject;
        let ctx = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }?;
        let mut env = vm.attach_current_thread()?;
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
        env.call_method(&activity, "restartImeInput", "()V", &[])?;
        Ok(())
    }
    if let Err(e) = call() {
        // Non-fatal: the keyboard keeps its previous layout until it next
        // hides and shows.
        log::warn!("ime: restartImeInput failed: {e}");
    }
}

/// `MainActivity.onImeInsetsChanged` override → keyboard height in physical
/// pixels (0 when it hides). Runs on the Android UI thread; the next egui
/// frame reads it via [`inset_bottom_points`].
#[no_mangle]
pub extern "system" fn Java_ie_cannygreen_berapp_MainActivity_nativeOnImeInsets(
    _env: jni::JNIEnv,
    _class: jni::objects::JClass,
    bottom_px: jni::sys::jint,
) {
    IME_INSET_BOTTOM_PX.store(bottom_px, Ordering::Relaxed);
}
