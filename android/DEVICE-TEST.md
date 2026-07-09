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

## 3. ~~On-screen pads (typing)~~ SUPERSEDED — see §7 (GameActivity migration)

The custom pads are no longer rendered on Android (2026-07, native-keyboard
migration); they remain the desktop `BER_TOUCH=1` path only. Typing is now
tested by §7.

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

## 7. GameActivity migration + native keyboard (2026-07) — rollback: revert the two `gameactivity` commits

The activity base class changed (NativeActivity → GameActivity), so EVERY JNI
bridge needs re-validation even though none of their code changed, and the
native soft keyboard replaces the custom pads. Plan + details:
`.claude/plans/native-keyboard-gameactivity.md` (§5/§7).

**Boot & bridges (gate — do these before trusting anything else):**

- [ ] App installs, boots to Splash → Home, no crash (a wrong games-activity
      AAR version aborts at startup with a `RegisterNatives` fatal in logcat).
- [ ] Immersive mode still holds: system bars hidden, re-hidden on focus regain.
- [ ] **Camera**: add a photo → system camera opens, capture returns into the app.
- [ ] **Bluetooth laser**: paired DISTO connects; a reading lands in the number box.
- [ ] **Audio**: bug-report voice note records and plays back / uploads.
- [ ] **Keep-awake**: screen stays on while the app is in use.
- [ ] **Network watcher**: queued bug report flushes when WiFi returns.
- [ ] Permission prompts (BT / audio) still appear and grant correctly.
- [ ] Two-finger nav + stroke retraction unaffected (§2 spot-check).
- [ ] Backgrounding, rotate, lock/unlock, swipe-away-and-relaunch — no crashes.

**Native keyboard:**

- [ ] Tap a measurement field → **numeric** keyboard appears; digits + `.` land
      in the field; backspace deletes **pre-filled** text too.
- [ ] Key Details name/nickname → **text** keyboard; typing + autocomplete work.
- [ ] Keyboard **Done** commits like Enter (prompt fires its primary action).
- [ ] Keyboard never permanently covers the field: floating number box moves
      above it; form fields scroll into view.
- [ ] Dismiss the keyboard with Back → app keeps working; tapping the field
      again re-raises it.
- [ ] Keyboard hides when the prompt closes / focus leaves the field.
- [ ] The old pads do NOT appear anywhere on Android.
- [ ] Desktop `BER_TOUCH=1` still shows the pads (regression escape hatch).

**Cursor (mirrored both ways with the IME):**

- [ ] Tap **mid-text** in a filled field → typing inserts AT the caret, not the end.
- [ ] Backspace deletes the character **before the caret** (mid-text too).
- [ ] **Select a range** (long-press / drag handles or double-tap) → the next
      keystroke replaces the selection.
- [ ] Autocomplete/word suggestions still work when typing at the end.
- [ ] After an accepted suggestion the caret sits where the keyboard put it.

Known accepted quirks: `+` sums (e.g. `2.1+0.9`) may be missing from some
numeric keyboards — use the number box's own keypad for expressions.

---

## Notes / results

> Record failures here with the logcat excerpt and which fallback commit you bisected to.

- Rendering backend (glow vs alternative):
- Touch/stylus latency feel while sketching:
- Back button on Android 13:
- Other:
