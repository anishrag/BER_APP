# Plan: Native Android keyboard via GameActivity migration

**Status:** Not started. Written 2026-07-09 as an executable plan for a future agent.
**Motivating report:** `fee88446` — "the on-screen keyboard is pretty terrible — can we use the native keyboard?"
**Owner note:** This is a BACKEND MIGRATION, not a small patch. Do it staged; each phase is independently verifiable and independently revertable. Do NOT start Phase 2 until Phase 1 (app runs on GameActivity with every JNI bridge re-validated) is green on the tablet.

---

## 1. Goal

Replace the custom on-screen pads (`src/ui/widgets/pads.rs`) with the **native Android soft keyboard** so the assessor gets a real keyboard (numeric layout, autocomplete, editing) for dimension and text entry.

## 2. Why this is the only viable native path (2026 findings)

- The app renders its own keyboards in pure egui; there is **no native IME involvement at all** today (`pads.rs` doc: "Android's system IME for egui is unreliable — the pad guarantees text entry on-device").
- winit's Android **IME/soft-keyboard issue ([#1823](https://github.com/rust-windowing/winit/issues/1823) → [#3787](https://github.com/rust-windowing/winit/issues/3787)) is still OPEN as of Jan 2026** (blocked on a threading constraint). So a keyboard "just working" on the current stack is not available.
- The app runs on **NativeActivity** (`winit`/`eframe` feature `android-native-activity`; `MainActivity extends android.app.NativeActivity`). NativeActivity has **no `InputConnection`**, so even if you force the keyboard up (JNI `InputMethodManager.showSoftInput`), typed text is delivered via `commitText`, which NativeActivity never wires up → **text never reaches the app**. That path (old "option A") is a confirmed DEAD END. Do not attempt it.
- **GameActivity** (from `androidx.games:games-activity`) DOES have working native text input via **GameTextInput**: `GameActivity_showSoftInput()` / `hideSoftInput()` show the keyboard and typed text flows back through the app (`textInputState` / a text-changed event). winit merged soft-keyboard support for this backend ([PR #3993](https://github.com/rust-windowing/winit/issues/3787)) and can switch between NativeActivity and GameActivity backends. **This is the supported, proven native path — but it requires migrating the app off NativeActivity.**

Refs: [winit #1823](https://github.com/rust-windowing/winit/issues/1823), [winit #3787](https://github.com/rust-windowing/winit/issues/3787), [android-activity #44](https://github.com/rust-mobile/android-activity/issues/44), [GameActivity TextInput](https://developer.android.com/games/agdk/game-activity/use-text-input), [android-activity docs](https://github.com/rust-mobile/android-activity).

## 3. Current setup (grounded — verify these still hold before starting)

- `Cargo.toml`:
  - `egui = "0.33.3"`, `egui_extras = "0.33.3"`, `eframe = "0.33.3"`.
  - `[target.'cfg(target_os="android")'.dependencies]`: `winit = { version = "0.30", features = ["android-native-activity"] }`; `eframe = { …, default-features = false, features = ["android-native-activity"] }`.
  - `jni = "0.21"`, `ndk-context = "0.1"`. The Cargo.toml comment already anticipates this: "…so the eframe/x11 features are desktop-only… you use android-game-activity…".
  - `AndroidApp` is always named THROUGH winit (`winit::platform::android::activity::AndroidApp`) — never depend on `android-activity` directly (the version must be the one winit links).
- `src/android.rs` — `android_main(app: AndroidApp)` entry (cfg'd), passes `AndroidApp` opaquely into `ber_app::bootstrap`/`ui::run_android`. eframe wired only in `src/ui/mod.rs`.
- `android/app/src/main/java/ie/cannygreen/berapp/MainActivity.java` — **`extends android.app.NativeActivity`**, thin subclass adding: immersive mode (`hideSystemBars`), and the JNI bridges: **camera** (photos), **Bluetooth laser** (BluetoothGatt…), **audio** (`MediaRecorder`), **keep-awake**, permissions. `System.loadLibrary("ber_app")` loads the cdylib; several `native` methods + Java→native callbacks.
- `android/app/src/main/AndroidManifest.xml` — `<activity android:name=".MainActivity" … configChanges="orientation|keyboardHidden|keyboard|screenSize|screenLayout|uiMode">`; comment notes "NativeActivity loads libber_app.so … calls android_main".
- `android/build.gradle.kts`, `android/app/build.gradle.kts` — the Gradle project.
- Build: `scripts/build-android.sh` (cargo-ndk arm64 → `android/app/src/main/jniLibs/` → Gradle assemble). Deploy: `scripts/deploy-tablet.sh`. Toolchain: `JAVA_HOME=/home/anishrag/android-toolchain/jdk-17.0.12+7`.
- `src/ui/widgets/pads.rs` — the custom `metre_input` (numeric) / `text_input` (qwerty) pads; Android always, desktop via `BER_TOUCH=1`.

## 4. Migration — staged

### Phase 0 — Spike & de-risk (throwaway branch)
Before touching this repo's Android project, confirm the toolchain path works end-to-end:
1. Build `rust-mobile`/`inferrna`'s `hello_world_android_egui` (or a minimal eframe app) with `winit`/`eframe` `android-game-activity` features, on this machine's cargo-ndk + JDK17, and confirm it: builds, installs on the SM-P615 tablet, shows a native keyboard on a focused `TextEdit`, and typed text lands. This proves the egui 0.33 + winit 0.30 + GameActivity combo actually delivers text on THIS egui version (the crux risk — if egui 0.33 doesn't consume the GameTextInput events winit emits, you may need an egui bump; find that out here, cheaply).
2. If egui 0.33 does NOT surface the text: evaluate bumping egui/eframe/egui_extras to the latest 0.3x that does, as a prerequisite. Record the minimum version.

### Phase 1 — Switch the backend, keep behaviour identical (no keyboard change yet)
Goal: the app builds and runs on GameActivity with EVERY existing feature intact, still using the custom pads.
1. **Cargo.toml**: change the android-target `winit` and `eframe` features from `android-native-activity` → `android-game-activity`. Keep everything else. (`AndroidApp` type is the same symbol; the backing activity changes.)
2. **Gradle**: add the GameActivity dependency to `android/app/build.gradle.kts`: `implementation("androidx.games:games-activity:<latest>")` (check the current version compatible with the `android-activity` version winit 0.30 links — align them; mismatches here are a common failure). Ensure `minSdk`/`compileSdk`/NDK are compatible.
3. **MainActivity.java**: change `extends android.app.NativeActivity` → `extends com.google.androidgamesdk.GameActivity` (import it). GameActivity loads the native lib via a manifest `meta-data` (`android.app.lib_name = ber_app`) rather than NativeActivity's convention — set that. Keep all the camera/BT/audio/keep-awake/immersive code, but re-check any call that assumed the NativeActivity base (e.g. `getWindow()`, surface/insets, `onCreate` super call).
4. **AndroidManifest.xml**: keep `.MainActivity`; add `<meta-data android:name="android.app.lib_name" android:value="ber_app" />` inside the activity (GameActivity reads this). Re-check `configChanges` (GameActivity + IME wants `keyboardHidden|keyboard` handled — keep them; consider adding `navigation`). Update the manifest comment that currently says "NativeActivity loads libber_app.so".
5. **Immersive mode / insets**: GameActivity manages the window differently. Re-validate `hideSystemBars` and that the soft keyboard's insets don't fight immersive mode (you'll likely need `WindowInsets` handling so the keyboard doesn't get hidden by immersive full-screen — this is a known friction point; plan to let the IME show even in immersive mode).
6. **Build + deploy + regression test** (see §8) — MUST pass before Phase 2. The biggest risk is the JNI bridges; re-validate every one (§5).

### Phase 2 — Wire the native keyboard to egui focus
1. **Show/hide on focus**: each frame, read egui's `ctx.wants_keyboard_input()` (true when a `TextEdit` is focused). On the rising edge, request the soft keyboard; on the falling edge, hide it. The call is `AndroidApp::show_soft_input(show_implicit)` / `hide_soft_input()` — reachable via `winit::platform::android::activity::AndroidApp` (the handle already flows through `src/android.rs`). **Threading constraint (the winit #3787 blocker):** show/hide must run on the thread that owns the view — call it from the winit event-loop/main thread, NOT from a worker. Thread the `AndroidApp` handle (or a channel to the event loop) into the UI layer so `src/ui` can request show/hide at the right time. Keep this cfg'd to `target_os="android"`.
2. **Receive text**: with GameActivity + `android-game-activity`, winit emits the typed text as window text/IME events that egui-winit forwards into egui as `Event::Text` / key events (verified in Phase 0). Confirm the focused `TextEdit` receives them and the custom pad is no longer needed. If egui 0.33 needs a shim, apply the minimal one found in Phase 0.
3. **IME type per field**: dimension fields want a NUMERIC keyboard, name fields a text one. GameTextInput supports input-type hints; expose a per-field hint (numeric vs text) from the prompt definitions in `number_box.rs` (there's already a `metre_input` vs `text_input` split to key off). If the winit/GameActivity API doesn't surface input-type, ship numeric-vs-text as a follow-up and accept the default keyboard first.
4. **Keyboard covers the field**: when the IME shows, pan/scroll the focused field above the keyboard (use the IME inset height from `WindowInsets` / winit's reported keyboard rect). The floating number box (`number_box.rs`) is positioned over the canvas — reposition it above the keyboard when the IME is up.

### Phase 3 — Retire / gate the custom pads
1. Behind `cfg(target_os="android")`, stop rendering `pads.rs`'s on-screen pads for fields now served by the native keyboard. KEEP `pads.rs` compiled and available behind `BER_TOUCH=1` for desktop and as a fallback (do not delete — it's the escape hatch if the native path regresses).
2. Verify every text/number entry point (dimensions, BER name, nicknames, years, notes) uses the native keyboard and nothing is left orphaned on the pad.

## 5. JNI bridge re-validation checklist (the #1 risk)
`MainActivity.java` carries camera/BT/audio/keep-awake bridges built against NativeActivity. After Phase 1, on the tablet, re-verify each still works under GameActivity:
- [ ] `System.loadLibrary("ber_app")` + `android.app.lib_name` meta-data actually loads the cdylib and calls `android_main`.
- [ ] `ndk-context` returns a valid JavaVM + activity pointer (GameActivity registers these differently; confirm `ndk_context::android_context()` is populated — this underpins ALL JNI calls).
- [ ] **Camera / external photos** (`src/ui/widgets/photo.rs` ↔ MainActivity camera intent + `native` callback) — take a photo, confirm it returns.
- [ ] **Bluetooth laser** (BluetoothGatt… + native callback) — pair + read a distance.
- [ ] **Audio recording** (`MediaRecorder`) — the bug-report audio capture (`src/controller/app_controller/bug_report.rs` + `platform::audio`).
- [ ] **Keep-awake / immersive mode** — screen stays on; system bars hidden; keyboard still shows.
- [ ] Permissions flow (camera/BT/audio/location) still prompts and grants.
- [ ] Two-finger nav + stroke retraction (`src/ui/pages/sketch/touch.rs`) unaffected.

## 6. Risks & mitigations
- **egui 0.33 may not consume GameTextInput text** → Phase 0 spike decides; mitigation = bump egui to the minimum working 0.3x (re-run the full test suite + boundary scan; egui bumps can ripple through `src/ui`).
- **JNI bridges break under GameActivity** → biggest risk; §5 checklist gates Phase 2; keep the NativeActivity branch as rollback.
- **Threading crash on show/hide** (winit #3787's blocker) → always call show/hide from the event-loop thread; never a worker.
- **IME vs immersive-mode insets** → handle `WindowInsets`; let the IME show; reposition the focused field.
- **android-activity ↔ games-activity version mismatch** → pin `androidx.games:games-activity` to the version matching the `android-activity` crate winit 0.30 links.
- **Gradle/NDK** → GameActivity pulls a prefab AAR; ensure `scripts/build-android.sh` + Gradle resolve it for arm64.

## 7. Verification (on the SM-P615 tablet)
1. Focus a dimension field → native (ideally numeric) keyboard appears; typed digits land in the field; OK commits.
2. Focus BER name / nickname → text keyboard; typing works; special chars OK.
3. The keyboard never permanently hides the active field (auto-scroll/reposition works).
4. Full §5 JNI regression passes.
5. Backgrounding/rotation/lock-unlock don't crash the keyboard or the JNI bridges.
6. Fallback: `BER_TOUCH=1` desktop still shows the custom pad (unbroken).

## 8. Build / deploy / rollback
- Build without the tablet: `JAVA_HOME=/home/anishrag/android-toolchain/jdk-17.0.12+7 ./scripts/build-android.sh debug`, then `adb install -r android/app/build/outputs/apk/debug/app-debug.apk` + `adb shell am force-stop ie.cannygreen.berapp` (relaunch to load new code). Tablet is wifi-adb (`192.168.0.233`, PORT ROTATES — ask the user for the current one).
- **Rollback = revert the Cargo.toml features + MainActivity `extends` + manifest meta-data + Gradle dep** (all Phase 1). Do Phase 1 as its own commit so rollback is one `git revert`.

## 9. Effort estimate
- Phase 0 spike: ~0.5 day (decides feasibility + egui version).
- Phase 1 (backend switch + JNI re-validation): ~1–2 days (the JNI re-validation dominates).
- Phase 2 (keyboard wiring): ~1 day.
- Phase 3 (gate pads): ~0.5 day.
- **Total ~3–4 days**, front-loaded risk in Phase 0/1. If Phase 0 shows egui 0.33 can't consume the text and an egui bump is needed, add time for the egui upgrade + full regression.

## 10. Do-NOT list
- Do NOT attempt the NativeActivity + JNI `showSoftInput` hack (option A) — text never returns.
- Do NOT delete `pads.rs` — keep it as the desktop path + rollback.
- Do NOT depend on `android-activity` directly in Cargo.toml — always through winit.
- Do NOT start Phase 2 before the §5 JNI checklist is green on-device.
