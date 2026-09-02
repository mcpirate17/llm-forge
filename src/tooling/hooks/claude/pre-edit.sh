#!/usr/bin/env bash
# PreToolUse: deny research dumps into .current_work.md; otherwise the checklist.
set -euo pipefail
REPO_ROOT="${PROJECT_DIR:-$(dirname "$(dirname "$(dirname "$(dirname "$(readlink -f "$0")")")")")}"
export PYTHONPATH="$REPO_ROOT${PYTHONPATH:+:$PYTHONPATH}"
exec python3 -m conductor.current_work_guard
