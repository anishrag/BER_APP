#!/usr/bin/env bash
# Enforce architecture boundaries declared in .claude/rules/architecture-boundaries.md.
# Usage:
#   check-boundaries.sh <file>   — check one file (PostToolUse hook)
#   check-boundaries.sh          — scan all modified/added files (Stop hook)
# Exits 0 on pass, 2 on violation (sends reason back to Claude).

set -u

violations=()

starts_with() {
  case "$1" in "$2"*) return 0 ;; *) return 1 ;; esac
}

check_file() {
  local f="$1"
  [[ -z "$f" ]] && return 0
  # Normalize ./prefix
  f="${f#./}"
  # Only Rust source files
  [[ "$f" == *.rs ]] || return 0
  # File must exist on disk (skip deleted)
  [[ -f "$f" ]] || return 0

  # Rule 1: egui/eframe only in src/ui/** and render/** (ber_render is the
  # shared drawing core — egui-based UI code packaged as a library so BER_deap
  # renders with the same code; eframe stays out of it via its Cargo.toml).
  if ! starts_with "$f" "src/ui/" && ! starts_with "$f" "render/"; then
    local hit
    hit=$(grep -nE '^\s*use\s+(egui|eframe|egui_extras|egui_datepicker|egui_field_editor)(\s|::|;|$)' "$f" 2>/dev/null || true)
    if [[ -n "$hit" ]]; then
      violations+=("$f: UI deps (egui/eframe/egui_*) are confined to src/ui/. Move this code into src/ui/ or talk to the UI via UiEvent. See .claude/rules/architecture-boundaries.md.")
      while IFS= read -r line; do violations+=("    $line"); done <<< "$hit"
    fi
  fi

  # Rule 2: nalgebra only in domain/**
  if ! starts_with "$f" "domain/"; then
    local hit
    hit=$(grep -nE '^\s*use\s+nalgebra(\s|::|;|$)' "$f" 2>/dev/null || true)
    if [[ -n "$hit" ]]; then
      violations+=("$f: nalgebra is confined to domain/. Use Point2f32/Point2f64/Point3f64 from domain::points.")
      while IFS= read -r line; do violations+=("    $line"); done <<< "$hit"
    fi
  fi

  # Rule 3: no slint anywhere
  local hit
  hit=$(grep -nE '^\s*use\s+slint(\s|::|;|$)' "$f" 2>/dev/null || true)
  if [[ -n "$hit" ]]; then
    violations+=("$f: slint is not used in this project. Use egui via src/ui/.")
    while IFS= read -r line; do violations+=("    $line"); done <<< "$hit"
  fi

  # Rule 4: rusqlite only in src/engine/** (and tests/)
  if ! starts_with "$f" "src/engine/" && ! starts_with "$f" "tests/"; then
    local hit
    hit=$(grep -nE '^\s*use\s+rusqlite(\s|::|;|$)' "$f" 2>/dev/null || true)
    if [[ -n "$hit" ]]; then
      violations+=("$f: rusqlite is confined to src/engine/. Go through BerRepository.")
      while IFS= read -r line; do violations+=("    $line"); done <<< "$hit"
    fi
  fi
}

if [[ $# -ge 1 ]]; then
  check_file "$1"
else
  # Stop mode: scan modified + untracked files
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    f="${line:3}"
    # Handle renames "R  old -> new"
    [[ "$f" == *" -> "* ]] && f="${f##* -> }"
    check_file "$f"
  done < <(git status --porcelain 2>/dev/null)
fi

if [[ ${#violations[@]} -gt 0 ]]; then
  printf 'Architecture boundary violations:\n' >&2
  for v in "${violations[@]}"; do
    printf '  %s\n' "$v" >&2
  done
  exit 2
fi

exit 0
