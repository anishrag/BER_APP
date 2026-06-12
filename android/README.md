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
