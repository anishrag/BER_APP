//! Shared widgets used across pages.

pub mod hint;
#[cfg(target_os = "android")]
pub mod native_ime;
pub mod pads;
pub mod photo;
pub mod photo_annotate;

pub use hint::HintExt;
pub use pads::{metre_input, text_input, text_input_multiline, PadResponse};
pub use photo::{photo_button, photo_button_pair, PhotoConfig, PhotoQuality};
pub use photo_annotate::PhotoAnnotator;

/// The native soft keyboard's height in egui points — 0 when it's hidden and
/// always 0 on desktop. Cross-platform wrapper over
/// `native_ime::inset_bottom_points` so layout code (scroll viewports,
/// window constrain rects) doesn't repeat the cfg dance at every call site.
pub fn ime_inset_bottom_points(ctx: &eframe::egui::Context) -> f32 {
    #[cfg(target_os = "android")]
    {
        native_ime::inset_bottom_points(ctx)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = ctx;
        0.0
    }
}

/// Bottom space to reserve at the END of a `ScrollArea`'s content that hosts
/// text fields, so the content can scroll far enough for a bottom field to
/// clear the native keyboard: the part of THIS scroll viewport the keyboard
/// covers. Call INSIDE the scroll content (it reads the viewport from
/// `ui.clip_rect()`), typically `ui.add_space(keyboard_scroll_reserve(ui))`
/// as the last item. Self-correcting: 0 on desktop / keyboard hidden (strict
/// layout no-op), and ~0 when the viewport was already shrunk to stop at the
/// keyboard top (the workspace form sections) — only the actually-covered
/// span is reserved.
pub fn keyboard_scroll_reserve(ui: &eframe::egui::Ui) -> f32 {
    let inset = ime_inset_bottom_points(ui.ctx());
    if inset <= 0.0 {
        return 0.0;
    }
    let keyboard_top = ui.ctx().content_rect().bottom() - inset;
    (ui.clip_rect().bottom() - keyboard_top).clamp(0.0, inset)
}
