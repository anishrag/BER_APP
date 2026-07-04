//! Platform bridges for the tablet: microphone capture for spoken bug reports,
//! and a WiFi-up signal that nudges the bug-report uploader. Android talks to
//! `MainActivity` over JNI (same pattern as the camera bridge in
//! `ui::widgets::photo`); desktop provides inert stubs so the rest of the app is
//! platform-agnostic. UI-free and toolkit-free — safe to call from the controller.

pub mod audio {
    use std::path::Path;

    /// Whether spoken bug-report recording is available here (tablet only).
    pub fn is_supported() -> bool {
        cfg!(target_os = "android")
    }

    /// Start recording to `path` (`.m4a`). On Android this asks `MainActivity`
    /// to run a `MediaRecorder`; on desktop it's unsupported.
    #[cfg(target_os = "android")]
    pub fn start(path: &Path) -> Result<(), String> {
        imp::start(path)
    }

    /// Stop recording. Synchronous on Android — the file is finalized (and ready
    /// to read) by the time this returns.
    #[cfg(target_os = "android")]
    pub fn stop() -> Result<(), String> {
        imp::stop()
    }

    #[cfg(not(target_os = "android"))]
    pub fn start(_path: &Path) -> Result<(), String> {
        Err("Audio recording is only available on the tablet.".into())
    }

    #[cfg(not(target_os = "android"))]
    pub fn stop() -> Result<(), String> {
        Ok(())
    }

    #[cfg(target_os = "android")]
    mod imp {
        use jni::objects::{JObject, JValue};
        use std::path::Path;

        /// Calls `MainActivity.startAudioRecording(String) -> boolean`.
        pub fn start(path: &Path) -> Result<(), String> {
            let path = path.to_str().ok_or("recording path is not valid UTF-8")?;
            let ctx = ndk_context::android_context();
            // SAFETY: android-activity registers the JavaVM + activity with
            // ndk_context before android_main runs (see ui::widgets::photo).
            let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(err)?;
            let mut env = vm.attach_current_thread().map_err(err)?;
            let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
            let jpath = env.new_string(path).map_err(err)?;
            let jpath = JObject::from(jpath);
            let ok = env
                .call_method(
                    &activity,
                    "startAudioRecording",
                    "(Ljava/lang/String;)Z",
                    &[JValue::Object(&jpath)],
                )
                .map_err(err)?
                .z()
                .map_err(err)?;
            if ok {
                Ok(())
            } else {
                Err("Could not start recording — check the microphone permission.".into())
            }
        }

        /// Calls `MainActivity.stopAudioRecording()` (finalizes synchronously).
        pub fn stop() -> Result<(), String> {
            let ctx = ndk_context::android_context();
            let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(err)?;
            let mut env = vm.attach_current_thread().map_err(err)?;
            let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
            env.call_method(&activity, "stopAudioRecording", "()V", &[])
                .map_err(err)?;
            Ok(())
        }

        fn err(e: impl std::fmt::Display) -> String {
            format!("audio JNI: {e}")
        }
    }
}

pub mod net {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Set by the Android connectivity callback when a WiFi network validates;
    /// drained each frame by the UI loop to trigger an upload attempt. Coalesces
    /// multiple callbacks into a single pending flag.
    static NETWORK_UP: AtomicBool = AtomicBool::new(false);

    /// Consume the "WiFi came up" signal, if any. Returns true once per event.
    pub fn take_wifi_up() -> bool {
        NETWORK_UP.swap(false, Ordering::AcqRel)
    }

    /// Raise the signal (called from the JNI callback below).
    #[cfg(target_os = "android")]
    fn signal_wifi_up() {
        NETWORK_UP.store(true, Ordering::Release);
    }

    /// `MainActivity.nativeOnNetworkUp()` — a WiFi-capable network became
    /// available; the next UI frame drains this and drains the upload queue.
    #[cfg(target_os = "android")]
    #[no_mangle]
    pub extern "system" fn Java_ie_cannygreen_berapp_MainActivity_nativeOnNetworkUp(
        _env: jni::JNIEnv,
        _class: jni::objects::JClass,
    ) {
        log::info!("net: WiFi up — triggering bug-report upload");
        signal_wifi_up();
    }
}
