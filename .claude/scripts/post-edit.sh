#!/usr/bin/env bash
# Runs after each Edit/Write. Reads file_path from stdin JSON, formats .rs files,
# then enforces architecture boundaries. Exit 2 = boundary violation (blocks turn).

set -u

f=$(jq -r '.tool_response.filePath // .tool_input.file_path // empty' 2>/dev/null)
[[ -z "$f" ]] && exit 0

# Normalize absolute project paths to relative
case "$f" in
  /home/anish/WORK/BER_APP/*) f="${f#/home/anish/WORK/BER_APP/}" ;;
esac
f="${f#./}"

# Best-effort format on Rust sources
if [[ "$f" == *.rs ]] && [[ -f "$f" ]]; then
  rustfmt --edition 2021 "$f" 2>/dev/null || true
fi

# Hard-enforced boundary check
exec .claude/scripts/check-boundaries.sh "$f"
