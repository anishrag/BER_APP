//! Platform-aware widget hints. Hover doesn't exist on touch: on Android
//! the tooltip is a no-op (the text must never be load-bearing there); on
//! pointer platforms it's exactly `on_hover_text`. Detection is the compile
//! target — deterministic, no runtime pointer-capability sniffing.

pub trait HintExt {
    /// Tooltip on pointer platforms; no-op on touch (Android).
    fn hint(self, text: impl Into<egui::WidgetText>) -> egui::Response;
    /// `on_disabled_hover_text` on pointer platforms; no-op on touch.
    fn disabled_hint(self, text: impl Into<egui::WidgetText>) -> egui::Response;
}

impl HintExt for egui::Response {
    fn hint(self, text: impl Into<egui::WidgetText>) -> egui::Response {
        if cfg!(target_os = "android") {
            self
        } else {
            self.on_hover_text(text)
        }
    }

    fn disabled_hint(self, text: impl Into<egui::WidgetText>) -> egui::Response {
        if cfg!(target_os = "android") {
            self
        } else {
            self.on_disabled_hover_text(text)
        }
    }
}
