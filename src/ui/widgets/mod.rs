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
