//! Two-finger navigation on the sketch canvas: pinch zooms, two-finger drag
//! pans, and a stroke the first finger started before the second landed is
//! retracted ([`SketchEvent::CancelStroke`]). One-finger input keeps flowing
//! through the canvas's existing primary-drag handlers untouched (on desktop
//! `multi_touch()` is always `None`, so this is inert under a mouse).

use super::SketchEvent;
use crate::ui::pages::events::UiEvent;

/// Cross-frame state for two-finger canvas navigation. Lives on
/// `SketchState` (the page's cross-frame UI state on `BerApp`).
#[derive(Default)]
pub struct TouchNav {
    /// True from the frame a two-finger gesture is adopted until EVERY
    /// finger lifts — egui's `multi_touch()` goes `None` as soon as the
    /// second finger lifts, but releasing one of two fingers must not
    /// resume drawing mid-gesture.
    nav_active: bool,
}

impl TouchNav {
    /// Per-frame, before the canvas's primary-drag handlers. On a
    /// multi-touch gesture that begins over the canvas this emits the
    /// existing [`SketchEvent::ZoomView`]/[`SketchEvent::PanView`] events
    /// from the gesture's per-frame deltas, and a `CancelStroke` (once, at
    /// adoption) if a stroke is in flight. Returns true while the caller
    /// must suppress primary-drag input (strokes, Move drags, exposure
    /// drags, 3D orbit) — held through the gesture's final frame so a
    /// trailing `drag_stopped` can't fire `EndStroke` on the cancelled
    /// stroke.
    pub(super) fn handle(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        stroke_in_flight: bool,
        on_event: &mut impl FnMut(UiEvent),
    ) -> bool {
        if let Some(mt) = ui.input(|i| i.multi_touch()) {
            if !self.nav_active {
                // Only adopt a gesture centred over the canvas (not the
                // category panel / prompt dock); once adopted it stays ours
                // even if the centre drifts out of the rect.
                if !rect.contains(mt.center_pos) {
                    return false;
                }
                self.nav_active = true;
                if stroke_in_flight {
                    on_event(UiEvent::Sketch(SketchEvent::CancelStroke));
                }
            }
            // Per-frame deltas, same convention as the scroll-zoom and
            // middle-drag-pan paths.
            if (mt.zoom_delta - 1.0).abs() > f32::EPSILON {
                on_event(UiEvent::Sketch(SketchEvent::ZoomView {
                    factor: mt.zoom_delta,
                    anchor_x: mt.center_pos.x,
                    anchor_y: mt.center_pos.y,
                    canvas_center_x: rect.center().x,
                    canvas_center_y: rect.center().y,
                }));
            }
            let d = mt.translation_delta;
            if d.x.abs() > 0.0 || d.y.abs() > 0.0 {
                on_event(UiEvent::Sketch(SketchEvent::PanView { dx: d.x, dy: d.y }));
            }
            true
        } else if self.nav_active {
            if !ui.input(|i| i.any_touches()) {
                self.nav_active = false;
            }
            true
        } else {
            false
        }
    }
}
