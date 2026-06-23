//! Shared widgets used across pages.

pub mod hint;
pub mod pads;
pub mod photo;

pub use hint::HintExt;
pub use pads::{metre_input, text_input, PadResponse};
pub use photo::{photo_button, PhotoConfig, PhotoQuality};
