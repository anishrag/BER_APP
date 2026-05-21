#!/usr/bin/env bash
# Block edits to files Claude shouldn't touch directly.
# Reads tool_input.file_path from stdin JSON. Exits 2 with reason to refuse.

set -u

f=$(jq -r '.tool_input.file_path // empty' 2>/dev/null)
[[ -z "$f" ]] && exit 0

# Normalize: strip leading ./
f="${f#./}"
# Strip absolute path prefix matching project root
case "$f" in
  /home/anish/WORK/BER_APP/*) f="${f#/home/anish/WORK/BER_APP/}" ;;
esac

case "$f" in
  Cargo.lock)
    echo "Refusing to edit Cargo.lock directly — it's managed by cargo. Add the dep to Cargo.toml and run \`cargo build\`/\`cargo update\` instead." >&2
    exit 2
    ;;
  target/*)
    echo "Refusing to edit $f — target/ is cargo's build output and should not be modified." >&2
    exit 2
    ;;
  .git/*)
    echo "Refusing to edit $f — .git/ is the git internal directory. Use git commands instead." >&2
    exit 2
    ;;
esac

exit 0
