//! MODULAR photo-attach button — reused from every site that collects photos
//! (construction build-ups, surfaces, openings, …).
//!
//! - **Desktop**: opens a native "choose image" dialog (`rfd`) and returns the
//!   chosen file path synchronously; the caller's controller copies it into the
//!   photo store.
//! - **Android**: launches the system camera (flash + quality are the camera
//!   app's own controls) and returns the captured photo's path. The camera is a
//!   *separate activity*, so the result arrives asynchronously — the button
//!   yields `Some(path)` on whatever later frame the capture lands (see
//!   [`android_cam`]). The capture's *default quality* is supplied per call site
//!   via [`PhotoConfig`] and forwarded to the activity for future use.
//!
//! Each call site renders [`photo_button`], reads the returned source path, and
//! emits its own "attach photo" event — the storage + attachment live in the
//! controller, so this widget stays a thin, reusable trigger.

use std::path::PathBuf;

/// Capture-quality tier, chosen per call site (a build-up close-up wants higher
/// quality than a context shot). The tier lives controller-side — it drives how
/// the captured file is recompressed into the photo store — and is re-exported
/// here so the widget and the Android JNI bridge below share one definition.
pub use crate::controller::app_controller::PhotoQuality;

/// Per-call-site photo configuration. `default_quality` seeds the camera's
/// quality (the assessor can still change it, along with flash, on the camera
/// screen).
#[derive(Debug, Clone, Copy)]
pub struct PhotoConfig {
    pub default_quality: PhotoQuality,
}

/// A reusable "add photo" button. Returns `Some(path)` on the frame a photo
/// becomes available — synchronously on desktop (a chosen file), or on a later
/// frame on Android (a captured file, once the camera activity returns). The
/// caller copies/attaches the file (via its controller); this only triggers the
/// platform capture.
pub fn photo_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    config: PhotoConfig,
) -> Option<PathBuf> {
    photo_button_with_hint(ui, label, "Add photo", enabled, config, false)
}

/// TEMPORARY (dual hi/lo capture, 2026-07): every photo-attach site renders TWO
/// capture buttons through this ONE helper — the site's usual button (`label`,
/// capturing at the site's `hi_quality` tier) plus a compact `📷·lo` button
/// that captures the same way but is ingested at [`PhotoQuality::Low`]
/// (downscaled + recompressed to a small JPEG on EVERY platform — see
/// `PhotoQuality::compression`). Returns the captured/picked path together with
/// the tier the pressed button asks for; the caller forwards both to its
/// attach event so the controller's shared `ingest_photo` does the (optional)
/// recompress. A smarter size policy replaces this later — being one shared
/// helper, removal is this function plus the Low-tier tuning it leans on.
pub fn photo_button_pair(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    hi_quality: PhotoQuality,
    auto_open: bool,
) -> Option<(PathBuf, PhotoQuality)> {
    let hi = photo_button_with_hint(
        ui,
        label,
        "Add photo",
        enabled,
        PhotoConfig {
            default_quality: hi_quality,
        },
        // Auto-launch the HI capture only (a required photo shouldn't force the
        // low-res tier). The caller latches this to one frame per selection.
        auto_open,
    )
    .map(|p| (p, hi_quality));
    let lo = photo_button_with_hint(
        ui,
        "📷·lo",
        "Add photo (low-res — small file)",
        enabled,
        PhotoConfig {
            default_quality: PhotoQuality::Low,
        },
        false,
    )
    .map(|p| (p, PhotoQuality::Low));
    // At most one button can deliver a path on a given frame (desktop dialogs
    // are modal; Android routes by button token), so `or` never drops a capture.
    hi.or(lo)
}

/// [`photo_button`] with a custom hover hint (the dual-capture pair uses this
/// to differentiate the low-res button).
fn photo_button_with_hint(
    ui: &mut egui::Ui,
    label: &str,
    hint: &str,
    enabled: bool,
    config: PhotoConfig,
    auto_open: bool,
) -> Option<PathBuf> {
    let resp = ui
        .add_enabled(enabled, egui::Button::new(label))
        .on_hover_text(hint);
    // `auto_open` fires the capture as if the button were clicked — the caller
    // uses it for a mandatory photo, latched to a single frame per selection so
    // it never re-launches after a cancel.
    let clicked = resp.clicked() || (enabled && auto_open);

    // Android delivers asynchronously: pick up a finished capture on whatever
    // frame it lands (the camera ran as a separate activity in the meantime).
    // The capture is keyed to THIS button's stable id so that when several
    // `photo_button`s render in one frame (e.g. the four external-photo sides),
    // only the one that launched the camera claims the result — not whichever
    // happens to render first.
    #[cfg(target_os = "android")]
    {
        let token = resp.id.value();
        if let Some(path) = android_cam::poll(token) {
            return Some(path);
        }
        if clicked {
            android_cam::begin(config.default_quality, token);
        }
        return None;
    }
    #[cfg(not(target_os = "android"))]
    {
        if clicked {
            return capture(config);
        }
        None
    }
}

/// Desktop: a native file-open dialog filtered to images.
#[cfg(not(target_os = "android"))]
fn capture(_config: PhotoConfig) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Image", &["jpg", "jpeg", "png", "heic", "webp", "bmp"])
        .set_title("Add photo")
        .pick_file()
}

/// Android: launch the system camera via the activity and receive the captured
/// file path back asynchronously.
///
/// The flow mirrors the Bluetooth-laser bridge (`number_entry::bluetooth`): a
/// small global slot the UI polls each frame. The difference is direction — here
/// Rust also *calls into* Java (`MainActivity.requestPhoto`) to start the
/// camera, then the activity's `onActivityResult` saves the photo and calls back
/// through [`Java_ie_cannygreen_berapp_MainActivity_nativeOnPhotoCaptured`].
#[cfg(target_os = "android")]
pub mod android_cam {
    use super::PhotoQuality;
    use jni::objects::{JClass, JObject, JString, JValue};
    use jni::JNIEnv;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// A single in-flight capture at a time (the camera is modal). `in_flight`
    /// dedups repeated taps while the camera is open; `result` carries the path
    /// back to the polling button; `token` is the id of the button that launched
    /// the in-flight (or just-delivered) capture, so a multi-button frame routes
    /// the result to the right caller.
    #[derive(Default)]
    struct Slot {
        in_flight: bool,
        token: u64,
        result: Option<PathBuf>,
    }

    static SLOT: Mutex<Slot> = Mutex::new(Slot {
        in_flight: false,
        token: 0,
        result: None,
    });

    /// Begin a capture for the button identified by `token`: ask the activity to
    /// launch the camera. Idempotent while one is already in flight. The result
    /// arrives later via [`poll`], tagged with this `token`.
    pub fn begin(quality: PhotoQuality, token: u64) {
        {
            let mut slot = SLOT.lock().unwrap();
            if slot.in_flight {
                return;
            }
            slot.in_flight = true;
            slot.token = token;
            slot.result = None;
        }
        if let Err(e) = launch(quality.level()) {
            log::error!("camera: failed to launch capture: {e}");
            SLOT.lock().unwrap().in_flight = false;
        }
    }

    /// Take a delivered capture, if any, but only for the button that launched it
    /// (`token` matches). Consumed once.
    pub fn poll(token: u64) -> Option<PathBuf> {
        let mut slot = SLOT.lock().unwrap();
        if slot.token == token && slot.result.is_some() {
            slot.result.take()
        } else {
            None
        }
    }

    /// Call `MainActivity.requestPhoto(int)` over JNI to start the camera.
    fn launch(quality: i32) -> Result<(), jni::errors::Error> {
        // SAFETY: android-activity registers the JavaVM + activity with
        // `ndk_context` before `android_main` runs (so before any UI/button).
        let ctx = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }?;
        let mut env = vm.attach_current_thread()?;
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
        env.call_method(&activity, "requestPhoto", "(I)V", &[JValue::Int(quality)])?;
        Ok(())
    }

    /// `MainActivity.nativeOnPhotoCaptured(String)` — the camera saved a photo
    /// at `path` (a private file the activity copied it into). The next frame's
    /// `photo_button` poll picks it up; the app resuming after the camera
    /// activity naturally repaints.
    #[no_mangle]
    pub extern "system" fn Java_ie_cannygreen_berapp_MainActivity_nativeOnPhotoCaptured(
        mut env: JNIEnv,
        _class: JClass,
        path: JString,
    ) {
        let path: String = match env.get_string(&path) {
            Ok(s) => s.into(),
            Err(e) => {
                log::error!("camera: bad path string from activity: {e}");
                let mut slot = SLOT.lock().unwrap();
                slot.in_flight = false;
                return;
            }
        };
        log::info!("camera: captured {path}");
        let mut slot = SLOT.lock().unwrap();
        slot.result = Some(PathBuf::from(path));
        slot.in_flight = false;
    }

    /// `MainActivity.nativeOnPhotoCancelled()` — the user backed out / capture
    /// failed; clear the in-flight flag so the button can be tapped again.
    #[no_mangle]
    pub extern "system" fn Java_ie_cannygreen_berapp_MainActivity_nativeOnPhotoCancelled(
        _env: JNIEnv,
        _class: JClass,
    ) {
        log::info!("camera: capture cancelled");
        SLOT.lock().unwrap().in_flight = false;
    }
}

// ── Global photo lightbox ───────────────────────────────────────────────────

/// Memory key holding the OPEN lightbox photo's full-res URI (`None` closed).
const VIEWER_URI_ID: &str = "ber_photo_viewer_uri";

/// Open the full-screen photo viewer on `uri` (an egui image URI, e.g.
/// `file:///…`). Called by [`clickable_photo`]; usable directly by any site
/// with its own image widget.
pub fn open_photo_viewer(ctx: &egui::Context, uri: String) {
    ctx.data_mut(|d| {
        d.insert_temp(egui::Id::new(VIEWER_URI_ID), uri);
        // Every photo opens at FIT — zoom/pan never carry across photos.
        d.remove::<(f32, f32, f32)>(egui::Id::new(VIEWER_ZOOM_ID));
    });
}

/// A photo THUMBNAIL that opens the full-screen viewer when tapped — the
/// app-wide convention for every evidence/pending photo. `thumb_uri` renders
/// small; the viewer opens `full_uri` (pass the same value when there's no
/// separate full-res file). Returns the thumbnail's response.
pub fn clickable_photo(
    ui: &mut egui::Ui,
    thumb_uri: &str,
    full_uri: &str,
    size: egui::Vec2,
) -> egui::Response {
    let resp = ui
        .add(
            egui::Image::new(thumb_uri.to_string())
                .fit_to_exact_size(size)
                .maintain_aspect_ratio(true)
                .corner_radius(3.0)
                .sense(egui::Sense::click()),
        )
        .on_hover_text("Tap to view large");
    if resp.clicked() {
        open_photo_viewer(ui.ctx(), full_uri.to_string());
    }
    resp
}

/// Memory key holding the viewer's `(zoom, pan_x, pan_y)`. Reset on open.
const VIEWER_ZOOM_ID: &str = "ber_photo_viewer_zoom";

/// The full-screen viewer itself — call ONCE per frame at the app level,
/// after the pages, so it overlays everything. A dark backdrop with the
/// photo fitted large; the ✕ button (or tapping anywhere outside the photo)
/// closes it. ZOOM: pinch (or scroll / ctrl+scroll) zooms about the pointer,
/// dragging pans while zoomed, and a double-tap toggles fit ↔ 2.5×.
pub fn render_photo_viewer(ctx: &egui::Context) {
    let id = egui::Id::new(VIEWER_URI_ID);
    let Some(uri) = ctx.data(|d| d.get_temp::<String>(id)) else {
        return;
    };
    let zoom_id = egui::Id::new(VIEWER_ZOOM_ID);
    let (mut zoom, mut pan_x, mut pan_y) = ctx
        .data(|d| d.get_temp::<(f32, f32, f32)>(zoom_id))
        .unwrap_or((1.0, 0.0, 0.0));
    let screen = ctx.content_rect();
    let mut close = false;
    egui::Area::new(egui::Id::new("ber_photo_viewer_overlay"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            // Backdrop: swallow the whole screen; a click outside closes.
            let backdrop = ui.allocate_rect(screen, egui::Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(235));

            // Pinch / ctrl+scroll (`zoom_delta`) or plain scroll — the viewer
            // is modal, so the wheel has no other meaning here. Zoom about
            // the POINTER: the point under the finger stays put.
            let (zoom_delta, scroll_y, pointer) = ui.input(|i| {
                (
                    i.zoom_delta(),
                    i.raw_scroll_delta.y,
                    i.pointer.interact_pos(),
                )
            });
            let mut factor = zoom_delta;
            if scroll_y.abs() > 0.0 {
                factor *= (scroll_y * 0.002).exp();
            }
            if (factor - 1.0).abs() > 1e-4 {
                let new_zoom = (zoom * factor).clamp(1.0, 8.0);
                let applied = new_zoom / zoom;
                if let Some(p) = pointer {
                    // Keep the pointed-at image point stationary.
                    let c = screen.center() + egui::vec2(pan_x, pan_y);
                    let d = p - c;
                    pan_x += d.x * (1.0 - applied);
                    pan_y += d.y * (1.0 - applied);
                }
                zoom = new_zoom;
            }

            let img_rect = egui::Rect::from_center_size(
                screen.center() + egui::vec2(pan_x, pan_y),
                screen.size() * 0.92 * zoom,
            );
            let img_resp = ui.put(
                img_rect,
                egui::Image::new(uri.clone())
                    .max_size(img_rect.size())
                    .maintain_aspect_ratio(true)
                    .sense(egui::Sense::click_and_drag()),
            );
            // Drag pans (any zoom — at fit it just springs within the clamp).
            if img_resp.dragged() {
                let d = img_resp.drag_delta();
                pan_x += d.x;
                pan_y += d.y;
            }
            // Double-tap: toggle fit ↔ 2.5× about the tap point.
            if img_resp.double_clicked() {
                if zoom > 1.01 {
                    (zoom, pan_x, pan_y) = (1.0, 0.0, 0.0);
                } else if let Some(p) = img_resp.interact_pointer_pos() {
                    let d = p - screen.center();
                    zoom = 2.5;
                    pan_x = -d.x * (zoom - 1.0);
                    pan_y = -d.y * (zoom - 1.0);
                }
            }
            // Keep the image from flying away: the pan can offset at most
            // half the zoomed extent (at fit that pins it centred).
            let max_x = (screen.width() * 0.92 * (zoom - 1.0)) * 0.5;
            let max_y = (screen.height() * 0.92 * (zoom - 1.0)) * 0.5;
            pan_x = pan_x.clamp(-max_x.max(0.0), max_x.max(0.0));
            pan_y = pan_y.clamp(-max_y.max(0.0), max_y.max(0.0));

            if backdrop.clicked() && !img_resp.hovered() {
                close = true;
            }
            // Big ✕ target top-right ("X" text — glyph-safe on the tablet font).
            let x_rect = egui::Rect::from_center_size(
                screen.right_top() + egui::vec2(-36.0, 36.0),
                egui::vec2(48.0, 48.0),
            );
            if ui
                .put(
                    x_rect,
                    egui::Button::new(egui::RichText::new("X").size(26.0).strong()),
                )
                .clicked()
            {
                close = true;
            }
        });
    if close {
        ctx.data_mut(|d| {
            d.remove::<String>(id);
            d.remove::<(f32, f32, f32)>(zoom_id);
        });
    } else {
        ctx.data_mut(|d| d.insert_temp(zoom_id, (zoom, pan_x, pan_y)));
    }
}
