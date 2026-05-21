#!/usr/bin/env bash
# One-time setup: point this clone's git hooks at the repo-tracked .githooks/ directory.
# Re-run safely if you ever clone fresh or change the hooksPath.

set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

git config core.hooksPath .githooks
chmod +x .githooks/*

echo "✓ core.hooksPath set to .githooks"
echo "✓ Hook scripts made executable"
echo
echo "Active hooks:"
ls -1 .githooks/ | sed 's/^/  - /'
