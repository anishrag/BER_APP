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

/// Capture quality for the camera. Its DEFAULT is chosen per call site (e.g. a
/// build-up close-up wants higher quality than a context shot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoQuality {
    Low,
    Medium,
    High,
    Full,
}

impl PhotoQuality {
    /// Stable integer level passed across the JNI boundary to the activity.
    /// Kept in sync with `requestPhoto(int)` in `MainActivity.java`.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn level(self) -> i32 {
        match self {
            PhotoQuality::Low => 0,
            PhotoQuality::Medium => 1,
            PhotoQuality::High => 2,
            PhotoQuality::Full => 3,
        }
    }
}

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
    let clicked = ui
        .add_enabled(enabled, egui::Button::new(label))
        .on_hover_text("Add photo")
        .clicked();

    // Android delivers asynchronously: pick up a finished capture on whatever
    // frame it lands (the camera ran as a separate activity in the meantime).
    #[cfg(target_os = "android")]
    if let Some(path) = android_cam::poll() {
        return Some(path);
    }

    if clicked {
        #[cfg(not(target_os = "android"))]
        {
            return capture(config);
        }
        #[cfg(target_os = "android")]
        {
            android_cam::begin(config.default_quality);
        }
    }
    None
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
    /// back to the polling button.
    #[derive(Default)]
    struct Slot {
        in_flight: bool,
        result: Option<PathBuf>,
    }

    static SLOT: Mutex<Slot> = Mutex::new(Slot {
        in_flight: false,
        result: None,
    });

    /// Begin a capture: ask the activity to launch the camera. Idempotent while
    /// one is already in flight. The result arrives later via [`poll`].
    pub fn begin(quality: PhotoQuality) {
        {
            let mut slot = SLOT.lock().unwrap();
            if slot.in_flight {
                return;
            }
            slot.in_flight = true;
            slot.result = None;
        }
        if let Err(e) = launch(quality.level()) {
            log::error!("camera: failed to launch capture: {e}");
            SLOT.lock().unwrap().in_flight = false;
        }
    }

    /// Take a delivered capture, if any (consumed once).
    pub fn poll() -> Option<PathBuf> {
        SLOT.lock().unwrap().result.take()
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
