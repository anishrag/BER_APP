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
//! ([`inset_bottom_points`]) and covered fields scrolled into view.
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

/// The keyboard session: which field owns it and the last text state we
/// exchanged with the IME (so the mirror only reacts to real changes).
#[derive(Default)]
struct Session {
    owner: Option<egui::Id>,
    last_text: String,
    shown: bool,
    /// Inset the owner last scrolled itself into view for (px) — re-scroll
    /// only when the keyboard height actually changes, so the assessor can
    /// still scroll the form freely while typing.
    handled_inset_px: i32,
}

static SESSION: Mutex<Session> = Mutex::new(Session {
    owner: None,
    last_text: String::new(),
    shown: false,
    handled_inset_px: 0,
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

/// Attach the native keyboard to a `TextEdit`: call right after `ui.add(...)`
/// with the response and the field's backing buffer. Shows + seeds the IME on
/// focus gain, mirrors typed text back into `buf` while focused, and scrolls
/// the field above the keyboard when covered.
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

    if focused && !is_owner {
        // Field takes the keyboard: keyboard type first (it's read when the
        // IME binds on show), then seed the IME buffer with the field's
        // current text, cursor at the end, then request the keyboard.
        s.owner = Some(response.id);
        s.last_text = buf.clone();
        s.handled_inset_px = 0;
        set_editor_info(kind);
        push_state(app, buf);
        app.show_soft_input(false);
        s.shown = true;
    } else if focused && is_owner {
        if response.changed() {
            // The field was edited on the egui side this frame (e.g. paste);
            // push the new value so the IME buffer doesn't fight it.
            s.last_text = buf.clone();
            push_state(app, buf);
        } else {
            let state = app.text_input_state();
            if state.text != s.last_text {
                let mut text = state.text;
                if text.contains('\n') {
                    // Newline commit = this keyboard's "Done". Strip it (all
                    // participating fields are single-line) and re-push so the
                    // IME buffer matches what the field shows.
                    text.retain(|c| c != '\n');
                    out.submitted = true;
                    push_state(app, &text);
                }
                s.last_text = text.clone();
                if *buf != text {
                    *buf = text;
                    out.changed = true;
                }
            }
        }

        // Keyboard covers the field → scroll it into view (no-op outside a
        // ScrollArea). Once per keyboard-height change, not per frame.
        let inset_px = IME_INSET_BOTTOM_PX.load(Ordering::Relaxed);
        if inset_px > 0 && inset_px != s.handled_inset_px {
            s.handled_inset_px = inset_px;
            let visible_bottom =
                ui.ctx().content_rect().bottom() - inset_px as f32 / ui.ctx().pixels_per_point();
            if response.rect.bottom() > visible_bottom {
                ui.scroll_to_rect(response.rect, Some(egui::Align::Center));
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

/// Seed/replace the IME's text buffer with `text`, cursor at the end. Spans
/// are Java string (UTF-16) indices — count code units, not bytes.
fn push_state(app: &AndroidApp, text: &str) {
    let end = text.encode_utf16().count();
    app.set_text_input_state(TextInputState {
        text: text.to_owned(),
        selection: TextSpan { start: end, end },
        compose_region: None,
    });
}

// EditorInfo constants (android.text.InputType / android.view.inputmethod.EditorInfo).
const TYPE_CLASS_TEXT: i32 = 0x1;
const TYPE_CLASS_NUMBER: i32 = 0x2;
const TYPE_NUMBER_FLAG_SIGNED: i32 = 0x1000;
const TYPE_NUMBER_FLAG_DECIMAL: i32 = 0x2000;
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
    };
    let ime_options = IME_ACTION_DONE | IME_FLAG_NO_EXTRACT_UI | IME_FLAG_NO_FULLSCREEN;
    if let Err(e) = call_set_editor_info(input_type, IME_ACTION_DONE, ime_options) {
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
