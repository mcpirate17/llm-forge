#!/usr/bin/env bash
# PreToolUse/Bash: Block commands that destroy work or violate project policy.
#
# The deny rules live in forge's native bash_guard module, reached by
# _bash_guard.py for old settings. It matches at COMMAND POSITION
# (shlex tokenization + operator split + recursion into `bash -c`) rather than
# grepping the raw string. The old inline greps matched inside quotes, so a
# command that merely MENTIONED a banned pattern was denied -- see the header of
# _bash_guard.py for the 2026-08-08 incident that motivated the rewrite.
#
# Allowed commands fall through to _bash_impact.py, which surfaces the blast
# radius of borderline-destructive operations without blocking them.

set -euo pipefail

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PAYLOAD=$(cat)

allow() {
    echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}'
    exit 0
}

CMD=$(printf '%s' "$PAYLOAD" \
    | python3 -c "import sys,json; print(json.load(sys.stdin).get('tool_input',{}).get('command',''))" \
    2>/dev/null || true)

[ -z "$CMD" ] && allow

# ── Deny rules (command-position matched) ─────────────────────────────
if REASON=$(printf '%s' "$CMD" | python3 "$HOOK_DIR/_bash_guard.py"); then
    :  # exit 0 -> allowed, fall through to the impact analyzer
else
    STATUS=$?
    if [ "$STATUS" -ne 1 ] || [ -z "$REASON" ]; then
        echo "pre-bash: native guard failed (exit $STATUS); command not checked" >&2
        exit 2
    fi
    # Build the JSON in python so quotes/newlines in the reason cannot break it.
    REASON="$REASON" python3 -c '
import json, os
print(json.dumps({"hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": os.environ["REASON"].strip(),
}}))'
    exit 0
fi

# ── Impact analyzer for borderline-destructive commands ───────────────
# Soft-allow but force the impact (file count / size / row count) into the
# conversation, so the agent must surface it to the user before/after running.
if [ ! -x "$HOOK_DIR/_bash_impact.py" ]; then
    echo "pre-bash: native impact entrypoint missing: $HOOK_DIR/_bash_impact.py" >&2
    exit 2
fi
if printf '%s' "$PAYLOAD" | python3 "$HOOK_DIR/_bash_impact.py"; then
    exit 0
fi
echo "pre-bash: native impact analysis failed; command not checked" >&2
exit 2
