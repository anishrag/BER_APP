//! Keep the tablet screen on while the app is actively used, letting it sleep
//! only after a spell of no interaction.
//!
//! App-specific by construction: it toggles the activity's
//! `FLAG_KEEP_SCREEN_ON` window flag (via a JNI call to
//! `MainActivity.setKeepAwake`), which affects only this app's window — it never
//! touches the global screen-timeout setting. When the app goes idle we CLEAR
//! the flag, so the device's normal timeout takes back over and the screen can
//! sleep; the next touch re-arms it.
//!
//! On desktop the toggle is a no-op, so `tick` is safe to call everywhere.

use std::time::Duration;

/// Keep the screen on until this long after the last interaction (10 minutes).
const IDLE_SECS: f64 = 600.0;

#[derive(Default)]
pub struct KeepAwake {
    /// App-clock time (egui `input.time`) of the last user interaction.
    last_active: Option<f64>,
    /// Whether the keep-awake flag is currently set (avoid redundant JNI calls).
    on: bool,
}

impl KeepAwake {
    /// Call once per frame. Refreshes the idle timer on any user input and
    /// flips the keep-awake window flag when crossing the idle threshold.
    pub fn tick(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let last = *self.last_active.get_or_insert(now);
        let interacted = ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Touch { .. }
                        | egui::Event::PointerButton { .. }
                        | egui::Event::PointerMoved(_)
                        | egui::Event::MouseWheel { .. }
                        | egui::Event::Key { .. }
                        | egui::Event::Text(_)
                )
            })
        });
        if interacted {
            self.last_active = Some(now);
        }
        let idle = now - if interacted { now } else { last };
        let want = idle < IDLE_SECS;
        if want != self.on {
            self.on = want;
            set_keep_awake(want);
        }
        // egui is reactive: without a scheduled repaint an idle app never ticks
        // again, so it could never release the flag. While awake, wake up at the
        // idle deadline to re-evaluate.
        if self.on {
            ctx.request_repaint_after(Duration::from_secs_f64((IDLE_SECS - idle).max(0.0)));
        }
    }
}

/// Set/clear the activity's keep-screen-on flag over JNI (mirrors the camera
/// bridge in `ui::widgets::photo`).
#[cfg(target_os = "android")]
fn set_keep_awake(on: bool) {
    use jni::objects::{JObject, JValue};
    let ctx = ndk_context::android_context();
    let res = (|| -> jni::errors::Result<()> {
        // SAFETY: android-activity registers the JavaVM + activity with
        // `ndk_context` before `android_main` runs, so both are valid here.
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }?;
        let mut env = vm.attach_current_thread()?;
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
        env.call_method(&activity, "setKeepAwake", "(Z)V", &[JValue::Bool(on as u8)])?;
        Ok(())
    })();
    if let Err(e) = res {
        log::warn!("keep_awake: JNI setKeepAwake failed: {e}");
    }
}

#[cfg(not(target_os = "android"))]
fn set_keep_awake(_on: bool) {}
