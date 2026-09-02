#!/usr/bin/env bash
# PostToolUse/Bash: bound the output the model sees. Any stdout/stderr field
# over 8 KB is replaced by head + elision marker + tail via updatedToolOutput;
# the full text is saved under $BASH_QUIET_SAVE_DIR (this repo sets that in
# .claude/hooks/project/env.sh; generic fallback is a tempdir).
# Response shape is preserved; small outputs are returned untouched.
set -euo pipefail
HOOK_DIR="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"
REPO_ROOT="${PROJECT_DIR:-$(dirname "$(dirname "$(dirname "$HOOK_DIR")")")}"
PROJECT_HOOK_DIR="${PROJECT_HOOK_DIR:-$REPO_ROOT/.claude/hooks/project}"
if [[ -r "$PROJECT_HOOK_DIR/env.sh" ]]; then
  # shellcheck source=/dev/null
  source "$PROJECT_HOOK_DIR/env.sh"
fi
export PROJECT_DIR="$REPO_ROOT"
exec python3 "$HOOK_DIR/_bash_quiet.py"
