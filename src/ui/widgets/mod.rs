//! Shared widgets used across pages.

pub mod hint;
#[cfg(target_os = "android")]
pub mod native_ime;
pub mod pads;
pub mod photo;
pub mod photo_annotate;

pub use hint::HintExt;
pub use pads::{metre_input, text_input, PadResponse};
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
