# Android packaging

Minimal Gradle wrapper around the Rust app: `cargo-ndk` builds
`libber_app.so` (the `cdylib` whose `android_main` lives in
`src/android.rs`), Gradle packages it into an APK with a plain
`NativeActivity` (no Java/Kotlin code — `android:hasCode="false"`).
Target: sideloaded arm64 tablet, no Play Store.

## One-time setup

1. Android SDK command-line tools + NDK, with `ANDROID_HOME` (and
   `ANDROID_NDK_HOME` if the NDK lives outside the SDK) exported.
2. `rustup target add aarch64-linux-android`
3. `cargo install cargo-ndk`
4. Generate the Gradle wrapper (not checked in — it contains a binary jar):
   `cd android && gradle wrapper` (any Gradle ≥ 8.7 works; the wrapper pins
   itself afterwards).

## Build & install

```sh
./scripts/build-android.sh            # debug APK
./scripts/build-android.sh release    # release APK (needs signing config)
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
adb logcat -s ber_app                 # the app's android_logger tag
```

## Release signing

Generate a keystore ONCE, keep it out of git (`*.keystore` is ignored):

```sh
keytool -genkeypair -v -keystore android/berapp.keystore \
  -alias berapp -keyalg RSA -keysize 2048 -validity 10000
```

Then wire it into `android/app/build.gradle.kts`'s `release` build type
(`signingConfigs` + `signingConfig`), or sign the unsigned APK manually with
`apksigner sign --ks android/berapp.keystore ...`.

## Bluetooth laser meter (Leica DISTO) — status: NOT yet device-verified

The number-entry box has a **Laser** input mode that reads distances from a
paired Leica DISTO over BLE. The input engine lives in `src/number_entry/`
(`bluetooth.rs`); the radio code is behind the `bluetooth` cargo feature
(`btleplug`). It is **compile-verified on desktop only** — the on-device path
below has not been run against the D2 yet.

What's done:
- UI mode toggle + a pure RECEIVING panel (no scan, no connect button): Laser
  mode auto-attaches to the already-paired DISTO among the OS's known devices
  and listens. Readings (LE `f32` metres on the DISTO distance characteristic)
  fill the expression buffer; the assessor presses the laser's own button to
  measure. So the meter must be paired/connected at the OS level first.
- `cargo check --features bluetooth` builds (uses BlueZ on the Linux dev box).

What's still needed to make it work on the tablet (each needs the device to
verify):
1. **Build with the feature:** add `--features bluetooth` to the `cargo ndk …`
   lines in `scripts/build-android.sh` (left off by default so the normal APK
   carries no radio stack).
2. **droidplug companion:** btleplug's Android backend needs its Java helper
   (`com.nonpolynomial.btleplug.android.impl.Manager` / the `droidplug` AAR) on
   the classpath, and `btleplug::platform::init(&env)` called once from JNI at
   startup. The APK is currently pure-`NativeActivity` (`hasCode=false`); this
   adds a DEX, so flip that and add the AAR in `android/app/build.gradle.kts`.
3. **Permissions + runtime grant:** add to `AndroidManifest.xml`
   `BLUETOOTH_SCAN` (with `usesPermissionFlags="neverForLocation"`),
   `BLUETOOTH_CONNECT` (API 31+), and for API ≤ 30 `BLUETOOTH`/`BLUETOOTH_ADMIN`
   + `ACCESS_FINE_LOCATION`. Android 12+ requires a *runtime* request for the
   first two before scanning.
4. **Pair** the DISTO in Android Settings first — Laser mode then auto-attaches
   to it (matched by the Leica vendor service UUID, name as fallback) and
   listens; there's no in-app scan or connect step.

## Notes

- The Rust side must keep naming `AndroidApp` through
  `winit::platform::android::activity::AndroidApp` so the android-activity
  glue version matches the one winit links (see Cargo.toml).
- `android.app.lib_name` in the manifest must equal the crate's lib name
  (`ber_app` → `libber_app.so`).
- Orientation is locked to landscape in the manifest (tablet sketching app);
  `configChanges` keeps incidental config flips from recreating the activity.
- If rendering misbehaves on the device's EGL (glow backend), switch the
  backend in `src/ui/mod.rs::launch` — that's the one line it was kept to.
