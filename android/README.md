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

## One-shot deploy (build + install + configure)

`scripts/deploy-tablet.sh` does the whole loop in one command: build the APK,
`adb install` it, and push the cloud config (`sync.json`) into the app's private
storage.

```sh
./scripts/deploy-tablet.sh            # debug (default)
./scripts/deploy-tablet.sh release    # release (needs signing config)
```

One-time: create `scripts/tablet.env` (git-ignored) with your secrets so you
never re-type them —

```sh
BER_SYNC_KEY=your-SURVEYOR_ACCESS_KEY      # the surveyor API shared key
BER_SYNC_URL=https://<api-id>.execute-api.us-east-1.amazonaws.com  # optional*
# BER_STACK=ber-intake     # optional (default)
# AWS_REGION=us-east-1     # optional (default)
```

\*If `BER_SYNC_URL` is omitted and the `aws` CLI is available, the script derives
it from the BERintake stack's `ApiUrl` output. Thereafter every run rebuilds and
redeploys, fully configured, with no extra steps.

## Cloud sync & bug-report upload (`sync.json`)

Both the offline-first **cloud sync** (pull confirmed jobs from the intake
backend) and the **bug-report upload** (the in-app 🐞 button) read the SAME
config file — so configuring one configures both. It lives in the app's private
internal storage at `files/sync.json` (i.e.
`/data/data/ie.cannygreen.berapp/files/sync.json`) and holds:

```json
{
  "baseUrl": "https://<api-id>.execute-api.us-east-1.amazonaws.com",
  "apiKey":  "<your SURVEYOR_ACCESS_KEY>"
}
```

- `baseUrl` = the API Gateway URL — the BERintake stack's `ApiUrl` output
  (`$default` stage, no `/prod` suffix):
  ```sh
  aws cloudformation describe-stacks --stack-name ber-intake --region us-east-1 \
    --query "Stacks[0].Outputs[?OutputKey=='ApiUrl'].OutputValue" --output text
  ```
- `apiKey` = the backend's `SURVEYOR_ACCESS_KEY` (the shared surveyor key).

The file is app-private, so `scripts/deploy-tablet.sh` pushes it via `adb run-as`
(works because the debug APK is debuggable). To place it by hand instead:

```sh
adb shell run-as ie.cannygreen.berapp sh -c 'mkdir -p files && cat > files/sync.json' < sync.json
adb shell run-as ie.cannygreen.berapp cat files/sync.json    # verify
```

Restart the app afterwards — the sync worker + bug uploader read the config at
startup. **Without `sync.json`** the app is fully usable offline: bug reports
capture to `files/bug-reports/` and upload automatically the next launch after a
config is present (WiFi-triggered via the connectivity callback, plus a periodic
retry). `sync.json` itself is git-ignored (it carries the key); `sync.json.example`
in the repo root is the committed template. On **desktop** the same file lives at
`./sync.json` next to `bers.db`, handy for testing the whole flow off-device.

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

## Camera (in-app photos) — status: built, NOT yet device-verified

Photo-attach buttons (e.g. Structure build-ups) use the system camera on
Android. The flow is the mirror image of the laser bridge — here Rust both
*calls into* Java and gets a callback:

1. Tapping "add photo" runs `ui::widgets::photo`'s Android path, which uses the
   `jni` crate + `ndk_context` (the JavaVM + activity android-activity
   registers) to call `MainActivity.requestPhoto(int quality)`.
2. `requestPhoto` launches `ACTION_IMAGE_CAPTURE` with a **MediaStore** output
   URI (no FileProvider/androidx, no storage permission). Flash + quality are
   the camera app's own controls; `quality` is the per-call-site default,
   forwarded for a future CameraX capture screen.
3. `onActivityResult` copies the capture into private app cache, deletes the
   gallery row (keeps the assessor's library clean), and calls back
   `nativeOnPhotoCaptured(String path)` (or `nativeOnPhotoCancelled()`).
4. Rust (`photo::android_cam`) stores the path in a global the next
   `photo_button` frame polls — the app naturally repaints when it resumes after
   the camera activity. The controller then copies the file into the photo store
   and makes a thumbnail, exactly as on desktop.

Manifest: **no CAMERA permission** (the camera app holds it; we only hand off an
intent), a `<queries>` entry for `IMAGE_CAPTURE` (API 30+ package visibility),
and an optional `camera.any` feature. If R8/minify is enabled for release, keep
the native-method names (the `-keep` rule above already covers `MainActivity`).
Verify on the tablet with `adb logcat -s ber_app` (look for `camera:` lines).

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
