#!/usr/bin/env bash
# PostToolUse/Bash: refresh the code-review-graph after Bash changed the tree.
#
# The main graph-update hook fires on Edit|Write|NotebookEdit. Bash was removed
# from that matcher on 2026-08-08 because it re-ran the update after EVERY
# command (`ls`, `git status`) at a 30s timeout. But git commands DO rewrite the
# working tree wholesale, and a stale graph is worse than a slow one: every
# subsequent semantic_search / query_graph answer silently describes code that is
# no longer on disk.
#
# So: narrow trigger, synchronous update. Matching is a deliberately dumb
# substring test -- unlike the deny hook, a false positive here costs one
# redundant refresh, not a blocked command, so precision is not worth the
# coupling to _bash_guard.py's tokenizer.

set -euo pipefail

HOOK_DIR="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
REPO_ROOT="${PROJECT_DIR:-$(cd "$HOOK_DIR/../../.." && pwd)}"
# shellcheck source=/dev/null
source "$HOOK_DIR/_runtime.sh"
hook_pythonpath "$REPO_ROOT"
PYTHON="$(hook_python "$REPO_ROOT")"
cd "$REPO_ROOT"

quiet() {
    echo '{"hookSpecificOutput":{"hookEventName":"PostToolUse"}}'
    exit 0
}

CMD=$(cat | python3 -c "import sys,json; print(json.load(sys.stdin).get('tool_input',{}).get('command',''))" 2>/dev/null || true)

[ -z "$CMD" ] && quiet

# git subcommands that rewrite tracked files in the working tree.
echo "$CMD" | grep -qE 'git\s+(checkout|switch|merge|rebase|stash|pull|reset|cherry-pick|revert|apply|am)\b' || quiet

if ! command -v code-review-graph >/dev/null 2>&1; then
    quiet
fi

if code-review-graph update --skip-flows >/dev/null 2>&1; then
    MSG="code-review-graph refreshed after a git working-tree change."
else
    MSG="WARNING: code-review-graph update FAILED after a git working-tree change. Graph reads are STALE until you run 'code-review-graph update' manually."
fi

MSG="$MSG" python3 -c '
import json, os
print(json.dumps({"hookSpecificOutput": {
    "hookEventName": "PostToolUse",
    "additionalContext": os.environ["MSG"],
}}))' | "$PYTHON" -m conductor.context_telemetry hook-context --hook post-bash-graph
