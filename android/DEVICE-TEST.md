# Android device-test checklist

The five `[device-test]` commits could not be verified from a build machine
(no tablet). HEAD contains all of their work, so one pass on the current build
exercises everything. Only `git checkout <commit>` to isolate a feature **if**
it misbehaves and you want to bisect.

**Setup:** keep `adb logcat -s ber_app` running in a second terminal the whole
session — when something fails, the log usually names the cause.

Test device of record: Samsung Galaxy Tab S6 Lite (SM-P615), updated off the
shipped Android 10 (`minSdk = 30` blocks install below Android 11).

---

## 1. Install & boot — fallback: `5d3973e`

- [ ] `adb devices` lists the tablet (SDK reads 30+).
- [ ] `adb install -r app/build/outputs/apk/debug/app-debug.apk` → `Success`.
- [ ] Launches to **Splash → Home** (New/Load visible); no immediate crash / black screen.
- [ ] **Landscape locked** — rotating doesn't reflow or restart the activity.
- [ ] UI renders cleanly — no garbled/blank GL.
      *(If garbled: swap the backend in `src/ui/mod.rs::launch`. Not a commit.)*
- [ ] `adb logcat -s ber_app` prints app log lines; no panics/aborts.

## 2. Sketching + two-finger navigation — fallback: `f40f535`

- [ ] One finger **draws** a stroke normally.
- [ ] **Pinch zooms**; two-finger **drag pans** the canvas.
- [ ] Second finger landing **mid-stroke retracts** the stroke (no half-line,
      no undo entry) and switches to navigate.
- [ ] **Lifting one of two fingers does NOT resume drawing** until every finger is up.
- [ ] In **3D view**, pinch **zooms without fighting the orbit** gesture.
- [ ] No phantom points / no unexpected "no active stroke" warning spam in logcat after a cancel.

## 3. On-screen pads (typing) — fallback: `8c50973`

- [ ] Measurement prompts open the **numeric metre pad**; `+` sums work (e.g. `2.1+0.9`).
- [ ] Pad **does not cover** the field being edited.
- [ ] **Key targets comfortable** for fingers (no fat-finger misses).
- [ ] Text fields (Key Details name + nickname) open **qwerty**; single-shot **shift** works.
- [ ] Numeric-only fields (years) get the numeric pad, not qwerty.
- [ ] **Check-mark submits** (equivalent to hardware Enter) and commits the value.

## 4. Android back button — fallback: `a292d60`

- [ ] Back from the **workspace → Home, edits saved** (not app exit).
- [ ] Back from **Home/Splash → minimises** the app (Android default), doesn't kill it.
- [ ] ⚠️ On **Android 13**, confirm back fires at all — predictive-back may change
      `KEYCODE_BACK` delivery (Phase-0 open question).

## 5. Hints absence — fallback: `ed01369`

- [ ] Nothing broken by tooltips being no-op on touch (gone by design).
- [ ] Watch the **disabled 2D/3D button**: is "set a ceiling height first" missable
      without hover? Flag for promotion to visible UI if confusing in practice.

## 6. Persistence round-trip — fallback: `5d3973e`

- [ ] **New** → create a record, enter the BER workspace, enter some data.
- [ ] Fully **close** the app (swipe from recents, not just background).
- [ ] **Reopen** → **Load** shows the record and it opens with data intact.

---

## Notes / results

> Record failures here with the logcat excerpt and which fallback commit you bisected to.

- Rendering backend (glow vs alternative):
- Touch/stylus latency feel while sketching:
- Back button on Android 13:
- Other:
