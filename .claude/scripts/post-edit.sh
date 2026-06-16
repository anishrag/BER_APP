#!/usr/bin/env bash
# Runs after each Edit/Write. Reads file_path from stdin JSON, formats .rs files,
# then enforces architecture boundaries. Exit 2 = boundary violation (blocks turn).

set -u

f=$(jq -r '.tool_response.filePath // .tool_input.file_path // empty' 2>/dev/null)
[[ -z "$f" ]] && exit 0

# Normalize absolute project paths to relative (repo root resolved dynamically,
# so this works across clones regardless of where the repo lives on disk).
repo_root=$(git rev-parse --show-toplevel 2>/dev/null)
if [[ -n "$repo_root" ]]; then
  f="${f#"$repo_root"/}"
fi
f="${f#./}"

# Best-effort format on Rust sources
if [[ "$f" == *.rs ]] && [[ -f "$f" ]]; then
  rustfmt --edition 2021 "$f" 2>/dev/null || true
fi

# Hard-enforced boundary check
exec .claude/scripts/check-boundaries.sh "$f"
