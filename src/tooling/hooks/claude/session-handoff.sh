#!/usr/bin/env bash
# SessionStart (startup|resume|clear|compact): the host adapter for
# conductor.inplace_handoff. If this identity staged a handoff envelope before a
# /clear, restart or compaction (`python -m conductor.inplace_handoff stage ...`),
# activate it and inject its bounded projection as the next-turn context. A
# prepared envelope is injected exactly once; a stale or tampered one is reported,
# never silently dropped. Emits an empty hook result when nothing is staged.
set -euo pipefail
HOOK_DIR="$(dirname "$(readlink -f "$0")")"
REPO_ROOT="${PROJECT_DIR:-$(dirname \
  "$(dirname \
    "$(dirname "$HOOK_DIR")")")}"
PYTHON="${HOOK_PYTHON:-$REPO_ROOT/.venv/bin/python}"
if [[ ! -x "$PYTHON" ]]; then PYTHON="$(command -v python3)"; fi
export PYTHONPATH="$REPO_ROOT${PYTHONPATH:+:$PYTHONPATH}"
# shellcheck source=/dev/null
source "$HOOK_DIR/_identity.sh"
cd "$REPO_ROOT"
"$PYTHON" -m conductor.inplace_handoff hook --identity "${A2A_ID:-claude}" \
  | "$PYTHON" -m conductor.context_telemetry hook-context --hook session-handoff --category instructions
