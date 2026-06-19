# Android port plan

**Status: active** (revised 2026-06-12; supersedes the postponed 2026-06-10 draft — the `gen_3d_vol` gate is moot, that crate was dissolved). Target: **sideloaded APK on an arm64 Android tablet**, no Play Store. Interface spec below was designed with the api-engineer (Mode 1) and verified against eframe 0.33.3 / egui 0.33.3 / winit 0.30.12 registry sources.

## Starting position (re-verified 2026-06-12)

- **Portable already:** all workspace crates (`domain`, `ber_model`, `sketch`, `manip_plan`, `document`) plus `src/controller` and `src/engine` have zero UI-toolkit imports. No native C deps anywhere except `rusqlite` `bundled` (NDK-safe). The only non-`src/` change in the whole port is one method on `sketch::SketchEngine`.
- **Toolkit facts (verified from registry sources, not memory):**
  - `eframe 0.33.3` has `NativeOptions.android_app: Option<winit::platform::android::activity::AndroidApp>` (cfg android).
  - eframe does **not** re-export winit's android module → the android entry needs a direct `winit` dep. `winit` is *not* on the boundary blocklist, so it may be named outside `src/ui`.
  - `egui 0.33.3` exposes `InputState::multi_touch() -> Option<MultiTouchInfo>` with `zoom_delta`, `translation_delta`, `center_pos`, `num_touches` — exactly the shape our `ZoomView`/`PanView` events need.
  - Android back button arrives as `egui::Key::BrowserBack` (winit maps `Keycode::Back` → `NamedKey::BrowserBack`; egui-winit forwards it). No raw winit plumbing needed.
  - `egui_virtual_keyboard` is stale (v0.1.3, pre-0.33 egui) — **do not adopt**; build a small custom numeric pad instead.
- **Current platform-bound spots:** fixed 800×600 viewport (`src/ui/mod.rs`); `bers.db` CWD-relative (`main.rs:25,29`); `tuning.yaml` via `$BER_TUNING` else cwd (`config.rs:72`); `$BER_CHECKPOINT` dev gate (`ui/app.rs:183`); `std::env::temp_dir()` only in a persistence *test*; ~116 `println!`/`eprintln!` in `src/` (86 in controller) vs ~10 `log::` calls; 67 hover sites (`prompt_dock.rs` 44, `categories.rs` 17, `canvas.rs` 3, `mod.rs` 3); 20 `TextEdit`s (14 numeric metre inputs in `prompt_dock.rs`, 4 free-text in `key_details.rs`, 2 in `categories.rs`).
- **Stale items from the old draft, dropped:** the dep-prune step (`slint`/`tiny-skia`/`simplify-polyline` are already gone; `earcutr` and `geo` are now genuinely used in `src/ui/render`); "~25 hover sites" (now 67); "~60 println" (now ~116).

---

## Phase 0 — Toolchain spike (scratch project, not this repo)

Goal: prove the toolchain and feel the device before touching BER_APP. ~1–2 days.

1. `rustup target add aarch64-linux-android`; Android SDK cmdline-tools + NDK; `ANDROID_HOME`/`ANDROID_NDK_ROOT`.
2. **Packaging tool decision:** try `xbuild` first (`x doctor` checks for system `lld`/`llvm-readobj`); **fallback is `cargo-ndk` + a minimal Gradle wrapper** — boring and stable if the pure-Rust packagers keep churning.
3. eframe hello-world APK: `#[no_mangle] fn android_main(app: AndroidApp)`, crate-type `cdylib`, eframe feature `android-native-activity`. Name `AndroidApp` **through winit** (`winit::platform::android::activity::AndroidApp`) — never depend on `android-activity` directly (version must match what winit links).
4. On the actual tablet, answer the **open device questions**:
   - Renderer: default glow vs wgpu on the device's EGL.
   - Does the system IME ever appear for an egui `TextEdit`? (Decides whether free-text fields need a custom keyboard or nothing.)
   - Does `multi_touch()` report correctly; does a stylus show up as a distinct touch device / pointer type? (If yes, "stylus draws / finger navigates" can replace the finger-count rule — `TouchNav` is the single place that policy lives.)
   - Is `KEYCODE_BACK` delivered as a key event by default, and how does Android 13+ predictive back interact?
   - `adb logcat` shows `android_logger` output.

**Exit criteria:** APK installs and runs; touch draws; renderer, packaging tool, IME, back-button, and stylus questions answered.

## Phase 1 — Entry point & platform plumbing (this repo)

Desktop build keeps working throughout; every slice below is desktop-safe to land now. Interface spec (api-engineer, Mode 1):

### Slice 7 first — log conversion (pure sweep, good first PR)
- `println!` → `log::debug!`/`info!`, `eprintln!` → `log::warn!`/`error!` across `src/` (~116 sites).
- Logger selection is an entry-point concern: `env_logger` stays in `main.rs`; `android_logger::init_once(...)` goes in `android_main`.
- **Trap:** the desktop default filter `error,document=debug,manip_plan=debug` would *silence* the converted controller lines — it must gain `ber_app=debug`.

### Slices 1+2 — paths, entry split, writer
- **New `src/paths.rs`:** `struct Paths { data_dir: PathBuf }` with `Paths::in_dir(PathBuf)` (Android), `Paths::desktop()` (`$XDG_DATA_HOME/ber_app` → `~/.local/share/ber_app` → cwd dev fallback; creates the dir), `db_file() -> PathBuf`. Path *policy* lives only in entry points — controller/document never see a path. No `dirs` crate.
- **`src/lib.rs`:** `pub fn bootstrap(paths: &Paths) -> Rc<RefCell<AppController>>` — open DB, build controller, attach `PersistenceWriter` (logged sync fallback on spawn failure). Panic on `open_db` failure (matches today; no recovery path on-device). Plus `#[cfg(target_os = "android")] mod android;`.
- **New `src/android.rs`** (whole file cfg'd; imports winit + android_logger only — **no egui/eframe**, boundary scan passes): `#[no_mangle] fn android_main(app: AndroidApp)` — init android_logger, `Paths::in_dir(app.internal_data_path().expect(...))`, `bootstrap`, `ui::run_android(controller, app)`.
- **`src/ui/mod.rs`** — the only place `NativeOptions` is built: keep `run()` for desktop; add `#[cfg(target_os="android")] pub fn run_android(controller, android_app)` setting `NativeOptions { android_app: Some(app), .. }` and **no fixed inner_size**. Both call a private `launch(controller, options)` (renderer choice is then one line).
- **`src/engine/persistence.rs`:** `PersistenceWriter::spawn(db_path: PathBuf)` (was `String`). **Trap:** `bootstrap` computes `paths.db_file()` once and passes the same value to `open_db` and `spawn` so they can't drift.
- **`src/engine/checkpoint.rs`: no change** — dev-only `$BER_CHECKPOINT` tool, naturally off on Android.
- **`Cargo.toml`:** `[lib] crate-type = ["rlib", "cdylib"]`; `[target.'cfg(target_os="android")'.dependencies]` winit (feature `android-native-activity`), `android_logger`, eframe feature `android-native-activity`.

### Slice 3 — tuning.yaml
- `src/config.rs`: `const EMBEDDED_TUNING: &str = include_str!("../tuning.yaml");` — `Tuning::load()` keeps its signature, resolution becomes `$BER_TUNING` file → `./tuning.yaml` → embedded copy. No path threading; `AppController::new(conn)` (40 call sites) untouched. Embedded parse error still falls back to `Tuning::default()` with `log::error!`.

**Exit criteria:** packaging tool produces an APK from this repo; app launches to Home on the tablet; a BER record persists and survives restart; desktop build/tests still green.

## Phase 2 — Touch interaction pass (`src/ui`)

### Slice 4 — gesture layer (the one real design problem)
- **New `src/ui/pages/sketch/touch.rs`:** retained `struct TouchNav { nav_active: bool }` on `BerApp`'s sketch state, with `handle(&mut self, ui, rect, stroke_in_flight, on_event) -> bool` called per-frame *before* the primary-drag stroke block. On `multi_touch()`: emits the **existing** `ZoomView` (factor = `zoom_delta`, anchor = `center_pos`) and `PanView` (`translation_delta`) events per frame, emits `CancelStroke` once if a stroke is in flight, returns true → caller suppresses Start/Add/EndStroke.
- **Rule:** cancel the in-flight stroke **whenever** a second finger lands — no N-ms grace window (strokes are cheap to redraw; keep a window only as a fallback knob if field use shows accidental cancels). **Trap:** `nav_active` must stay true until **zero** fingers remain, or lifting one of two fingers resumes drawing mid-gesture.
- **Cancel path (doesn't exist today), three small additions:**
  - `sketch/src/engine.rs`: `pub fn cancel_stroke(&mut self)` — abandon in-flight stroke, no-op if none. (The only non-`src/` change in the port.)
  - `SketchEvent::CancelStroke` (no payload) in `src/ui/pages/sketch/mod.rs`.
  - `src/controller/app_controller/strokes.rs`: `pub fn sketch_cancel_stroke(&mut self)` — nothing committed → **no undo snapshot**. Stroke preview clears for free (it's derived from the engine's stroke).
- Canvas change: stroke block at `canvas.rs:856` gains `&& !touch_nav_active`. Desktop-safe (`multi_touch()` is `None` with a mouse). No `document`/`manip_plan` changes.
- 3D orbit: one-finger drag already orbits; two-finger nav composes via the same gate.

### Slice 5 — numeric pad
- All 14 `prompt_dock.rs` inputs share one idiom (`TextEdit::singleline(&mut String).desired_width(80)` + Enter-submit + `parse_dimension_expression`) — that's the seam.
- **New `src/ui/widgets/metre_input.rs`:** `pub fn metre_input(ui, id, buf: &mut String, hint) -> MetreInputResponse { submitted }`. On touch builds, focusing it opens a shared private `NumPad` (digits, `.`, `+` — the parser accepts sums — backspace, ✓) anchored at screen bottom, editing the `&mut String` directly (no IME emulation, no event injection). Pad-owner id lives on `BerApp` per convention.
- Sweep the 14 sites + `categories.rs` 2 mechanically; events emitted unchanged, no controller involvement.
- Free-text fields (`key_details.rs` ×4, new-BER name) wait on the Phase-0 IME finding.

### Slice 6 — hover sweep
- **New `src/ui/widgets/hint.rs`:** `trait HintExt { fn hint(self, text) -> Response }` — tooltip on desktop, no-op on touch (`cfg!(target_os = "android")`; no runtime capability sniffing). Sweep 64 tooltip sites `.on_hover_text(t)` → `.hint(t)`.
- The 3 `canvas.rs` `hovered()` sites are *gates*, not tooltips (e.g. scroll-zoom gated on hover at `canvas.rs:645`) — audit individually; they must not gate the multi-touch path.
- Hovers whose text is load-bearing on a tablet get promoted to visible UI case-by-case — expect a handful.

### Slice 8 — back button
- Handled in `src/ui/app.rs` where navigation already lives: in `update`, check `egui::Key::BrowserBack`; private `fn back_target(page: Page) -> Option<Page>` (workspace tabs → Home; `Home → None` = let the activity minimise — **never** `process::exit`). Routes through existing `UiEvent::NavigateTo`; controller never learns about Android. Leave unconditional (reasonable desktop shortcut) unless it surprises.

### Also in this phase
- Viewport/DPI: drop the fixed 800×600 for Android (done via `run_android`); verify `pixels_per_point` on the tablet; bump `Style::spacing.interact_size` for finger targets.

**Exit criteria:** full sketch workflow (outline → dimension → exposure → living areas → storey → roof) completable with fingers only on the device.

## Phase 3 — Lifecycle robustness

1. **Suspend/resume:** background mid-sketch and resume — glow can lose the GL context; verify eframe 0.33's handling, switch `launch` to wgpu backend if broken (one line by design).
2. **Flush on pause:** force-flush the `PersistenceWriter` queue on suspend (coalesced writes must not be lost to a background kill). Verify checkpoint/snapshot recovery after force-kill.
3. **Rotation:** lock landscape in the manifest (it's a tablet sketching app); otherwise test resize mid-sketch.
4. **Permissions:** none (internal storage only). `ber_model::Photo` capture would be CAMERA + intent/JNI — out of scope.

## Phase 4 — Packaging & distribution

1. Manifest metadata (app id `ie.cannygreen.berapp`, label, icon, versionCode, orientation lock) via the chosen tool's config.
2. Release keystore once, out of git, document the signing command. Sideload: `adb install -r`.
3. `aarch64-linux-android` only.
4. Optional CI job: cross-compile (not package) for aarch64 on every push so desktop changes can't silently break the Android build.

## Phase 5 — Field acceptance checklist

On the device, one sitting: create BER → sketch ground floor → dimension via numeric pad → paint exposure → living area → upper storey → roof ridge → pinch-zoom throughout → background + resume mid-edit → force-kill + relaunch (snapshot recovery) → record on Home list.

## Implementation order & ownership

| Order | Slice | Files | Engineer | Desktop-safe now |
|---|---|---|---|---|
| 1 | 7 log sweep | `src/**` | any | yes |
| 2 | 1+2 paths/entry/writer | `src/paths.rs`, `lib.rs`, `android.rs`, `ui/mod.rs`, `main.rs`, `engine/persistence.rs` | shell-engineer | yes (cfg'd) |
| 3 | 3 tuning embed | `src/config.rs` | shell-engineer | yes |
| 4 | 4 gestures | `ui/pages/sketch/touch.rs`, `canvas.rs` gate; `sketch/src/engine.rs`; controller `strokes.rs` | shell + sketch one-liner | yes |
| 5 | 5 numeric pad | `src/ui/widgets/{mod,metre_input}.rs` + 16-site sweep | shell-engineer | yes |
| 6 | 6 hint sweep | `src/ui/widgets/hint.rs` + 64 sites | shell-engineer | yes |
| 7 | 8 back button | `src/ui/app.rs` | shell-engineer | yes |

ber-reviewer audits after Phases 1 and 2 land; live device runs stay with Anish.

## Risks & fallbacks

| Risk | Likelihood | Fallback |
| --- | --- | --- |
| Packaging tool churn (`cargo-apk` deprecated, `xbuild` contested) | High | `cargo-ndk` + minimal Gradle project |
| System IME absent for egui TextEdits | High (expected) | Numeric pad covers the field workflow; extend NumPad to a minimal qwerty for the few free-text fields |
| glow context loss on suspend | Medium | wgpu backend (one-line switch in `launch`) |
| eframe-on-Android fundamentally disappointing | Low | Native Kotlin shell + JNI driving `controller`/`document` as a library — `src/ui` rewrite is contained by the boundary rules |
| Parametric solve perf on tablet CPU | Low | Profile release builds in Phase 0/1; `manip_plan` is pure Rust |

## Open questions (flagged, answered in Phase 0)

1. System IME behaviour for egui TextEdits on the target tablet.
2. `AndroidApp::internal_data_path()` returning `None` — recommend panic with clear message.
3. Renderer choice (glow vs wgpu) on the device's EGL.
4. `KEYCODE_BACK` delivery + Android 13 predictive back.
5. Stylus reported as distinct touch device → could switch policy to stylus-draws/finger-navigates inside `TouchNav` with no new seams.
