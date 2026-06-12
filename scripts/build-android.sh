#!/usr/bin/env bash
# Build the Android APK: Rust cdylib via cargo-ndk, then package with Gradle.
# Prereqs (one-time): see android/README.md.
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="${1:-debug}" # debug | release

if ! command -v cargo-ndk >/dev/null 2>&1; then
    echo "cargo-ndk not found: cargo install cargo-ndk" >&2
    exit 1
fi
if [ ! -x android/gradlew ]; then
    echo "Gradle wrapper missing: run 'gradle wrapper' inside android/ once (see android/README.md)" >&2
    exit 1
fi

# 1. Cross-compile the cdylib into the Gradle project's jniLibs.
if [ "$PROFILE" = "release" ]; then
    cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build --release
else
    cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build
fi

# 2. Package the APK.
(cd android && ./gradlew "assemble${PROFILE^}")

APK="android/app/build/outputs/apk/${PROFILE}/app-${PROFILE}.apk"
echo
echo "APK: ${APK}"
echo "Install: adb install -r ${APK}"
