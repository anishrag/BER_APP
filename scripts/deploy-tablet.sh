#!/usr/bin/env bash
# One-shot tablet deploy: build the APK, install it, and push the cloud-sync /
# bug-upload config (sync.json) into the app's private storage. After a one-time
# setup (below) this is the only command you run to get a fresh build on the
# tablet, fully configured.
#
#   ./scripts/deploy-tablet.sh            # debug build (default)
#   ./scripts/deploy-tablet.sh release    # release build (needs signing config)
#
# Config resolution (for the surveyor API baseUrl + apiKey the tablet uses for
# both cloud sync AND bug-report upload — it's the same sync.json):
#   1. $SYNC_JSON pointing at an existing file, else ./sync.json if it exists.
#   2. Otherwise GENERATE ./sync.json from:
#        baseUrl : $BER_SYNC_URL, else the CloudFormation `ApiUrl` output of the
#                  BERintake stack (needs the aws CLI).
#        apiKey  : $BER_SYNC_KEY or $SURVEYOR_ACCESS_KEY.
#   3. If neither works, the APK still installs; reports queue on-device and
#      upload once a sync.json is present.
#
# Put your secrets in scripts/tablet.env (git-ignored) so you never re-type them:
#   BER_SYNC_KEY=...            # = SURVEYOR_ACCESS_KEY
#   BER_SYNC_URL=https://...    # optional; auto-derived from the stack if unset
#   BER_STACK=ber-intake        # optional (default)
#   AWS_REGION=us-east-1        # optional (default)
set -euo pipefail
cd "$(dirname "$0")/.."

PKG="ie.cannygreen.berapp"
PROFILE="${1:-debug}" # debug | release

# Optional secrets file (git-ignored) — export its vars for the run.
if [ -f scripts/tablet.env ]; then
    # shellcheck disable=SC1091
    set -a && . scripts/tablet.env && set +a
fi

require() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1 ($2)" >&2; exit 1; }; }
require adb "install the Android platform-tools"

if [ "$(adb get-state 2>/dev/null || true)" != "device" ]; then
    echo "No tablet connected (adb get-state != device). Plug in + enable USB debugging." >&2
    exit 1
fi

# ── 1. Build ────────────────────────────────────────────────────────────────
echo "==> Building ${PROFILE} APK"
./scripts/build-android.sh "$PROFILE"
APK="android/app/build/outputs/apk/${PROFILE}/app-${PROFILE}.apk"
[ -f "$APK" ] || { echo "APK not found at $APK" >&2; exit 1; }

# ── 2. Install ──────────────────────────────────────────────────────────────
echo "==> Installing $APK"
adb install -r "$APK"

# ── 3. Resolve / generate sync.json ─────────────────────────────────────────
CONFIG="${SYNC_JSON:-}"
if [ -z "$CONFIG" ] && [ -f sync.json ]; then
    CONFIG="sync.json"
fi
if [ -z "$CONFIG" ]; then
    # Try to generate ./sync.json from env + the deployed stack.
    base="${BER_SYNC_URL:-}"
    key="${BER_SYNC_KEY:-${SURVEYOR_ACCESS_KEY:-}}"
    if [ -z "$base" ] && command -v aws >/dev/null 2>&1; then
        echo "==> Deriving baseUrl from CloudFormation stack '${BER_STACK:-ber-intake}'"
        base="$(aws cloudformation describe-stacks \
            --stack-name "${BER_STACK:-ber-intake}" --region "${AWS_REGION:-us-east-1}" \
            --query "Stacks[0].Outputs[?OutputKey=='ApiUrl'].OutputValue" \
            --output text 2>/dev/null || true)"
    fi
    if [ -n "$base" ] && [ -n "$key" ]; then
        printf '{\n  "baseUrl": "%s",\n  "apiKey": "%s"\n}\n' "$base" "$key" > sync.json
        CONFIG="sync.json"
        echo "==> Generated sync.json (baseUrl=$base)"
    fi
fi

# ── 4. Push config into the app's private storage ───────────────────────────
if [ -n "$CONFIG" ] && [ -f "$CONFIG" ]; then
    echo "==> Pushing $CONFIG to $PKG (files/sync.json)"
    # run-as executes as the app uid; mkdir in case the app hasn't launched yet.
    adb shell run-as "$PKG" sh -c 'mkdir -p files && cat > files/sync.json' < "$CONFIG"
    echo "    verify:"
    adb shell run-as "$PKG" cat files/sync.json | sed 's/^/    /'
else
    echo "!! No sync.json resolved — APK installed but cloud upload is unconfigured."
    echo "   Reports will queue on-device and upload once a sync.json is present."
    echo "   Provide one via scripts/tablet.env, \$SYNC_JSON, or ./sync.json (see header)."
fi

echo
echo "Done. Launch the app on the tablet (config is read at startup)."
echo "Logs: adb logcat -s ber_app"
