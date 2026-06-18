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

## Bluetooth laser meter (Leica DISTO) — status: built, NOT yet device-verified

The number-entry box has a **Laser** input mode that reads distances from a
paired Leica DISTO over BLE. On **Android the BLE is done in Java**, not
btleplug: `MainActivity.java` connects to the bonded DISTO with `BluetoothGatt`,
subscribes to the distance characteristic (`3ab10101-…`), and pushes each
reading + a status code into the Rust app through two `native` methods
(`nativeOnDistance` / `nativeOnStatus`). The Rust side
(`src/number_entry/bluetooth.rs`, `#[cfg(target_os = "android")]` backend)
exposes those as `#[no_mangle] Java_ie_cannygreen_berapp_MainActivity_nativeOn*`
and feeds the same `LaserSource` the UI drains. Desktop keeps using `btleplug`
behind the `bluetooth` feature; **Android does NOT use that feature** (don't add
`--features bluetooth` to the NDK build — btleplug isn't cross-compiled).

Why Java, not btleplug's Android backend: btleplug needs its vendored
"droidplug" Gradle companion *plus* an unpublished `jni-utils` SNAPSHOT and a
JNI class-loader `init` — fiddly and unverifiable off-device. Android's own
`BluetoothGatt` + one JNI callback is far less moving parts.

What's wired up (needs the tablet + a paired D2 to actually verify):
- `MainActivity`: connect-on-resume / close-on-pause, `autoConnect` so it
  reconnects when the (battery-saving) Disto wakes; reports status codes.
- `AndroidManifest.xml`: `BLUETOOTH_CONNECT` (runtime-requested in
  `MainActivity` on API 31+) and legacy `BLUETOOTH` for API 30. **No scan, so no
  location permission** — we use the *bonded* device list.
- Rust Android backend + `nativeOn*` exports; `LaserSource.status()` drives the
  box's auto-switch into Laser mode once Connected.

To use on the tablet: **pair the DISTO in Android Settings first**, then build
and install normally (`./scripts/build-android.sh`); grant the Bluetooth prompt
on first launch. `adb logcat -s ber_app` shows the connect/subscribe/reading
trace. If R8/minify is ever enabled for release, keep the native-method names
(`-keep class ie.cannygreen.berapp.MainActivity { *; }`).

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
