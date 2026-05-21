#!/usr/bin/env bash
# Runs when Claude finishes a turn. Two gates:
#   1. Scan every modified/added file for boundary violations
#   2. cargo check --workspace must succeed
# Exit 2 with reason on any failure — Claude keeps working with the reason as next instruction.

set -uo pipefail

# Don't loop forever if Claude can't fix it — Claude Code caps blocked Stop hooks at 8.
# We rely on that cap rather than tracking stop_hook_active here.

# Gate 1: boundary scan over the working tree
if ! .claude/scripts/check-boundaries.sh; then
  exit 2
fi

# Gate 2: cargo check
# --message-format short keeps output compact; tail keeps the last 30 lines on failure
output=$(cargo check --workspace --message-format short --quiet 2>&1)
rc=$?
if [[ $rc -ne 0 ]]; then
  echo "cargo check --workspace failed (exit $rc). Fix compile errors before ending the turn." >&2
  echo "$output" | tail -30 >&2
  exit 2
fi

exit 0
