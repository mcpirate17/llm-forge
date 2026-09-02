#!/usr/bin/env bash
# PreToolUse/Read: deny a whole-file Read of a code file >= 400 lines (the
# KB-OPS-CTX-01 rule) with a reason that names the bounded alternatives: the
# graph's AST projection (mcp__code-review-graph__ast_context_tool),
# symbol_source_tool, or Read with offset/limit. Was advisory-only until
# 2026-09-01; 189 of 845 Reads still exceeded 8 KB with the nudge in place.
# Small files, non-code files and sliced reads pass untouched.
set -euo pipefail

# The heredoc below becomes python's stdin, so capture the hook payload first.
# hook-context logs the nudge size to the context telemetry and passes the JSON through.
HOOK_PAYLOAD="$(cat)" python3 - <<'PY' | python3 -m conductor.context_telemetry hook-context --hook pre-read-skeleton
import json, os, sys
from pathlib import Path

THRESHOLD_LINES = 400
CODE_SUFFIXES = {".py", ".rs", ".c", ".cc", ".cpp", ".h", ".hpp", ".ts", ".js", ".sh"}

def quiet() -> None:
    print('{"hookSpecificOutput":{"hookEventName":"PreToolUse"}}')
    sys.exit(0)

try:
    payload = json.loads(os.environ.get("HOOK_PAYLOAD", ""))
except json.JSONDecodeError:
    quiet()
if not isinstance(payload, dict):
    quiet()
tool_input = payload.get("tool_input") or payload.get("toolInput") or {}
path = tool_input.get("file_path") or tool_input.get("path") or ""
if not path or tool_input.get("offset") or tool_input.get("limit"):
    quiet()
file = Path(path)
if file.suffix not in CODE_SUFFIXES or not file.is_file():
    quiet()
try:
    data = file.read_bytes()
except OSError:
    quiet()
lines = data.count(b"\n")
if lines < THRESHOLD_LINES:
    quiet()
tokens = len(data) // 4
symbol_hint = " symbol=<name>" if file.suffix == ".py" else ""
msg = (
    f"PRE-READ DENIED: {file.name} is {lines} lines (~{tokens:,} tokens); whole-file "
    f"Read of a code file >= {THRESHOLD_LINES} lines is not allowed (KB-OPS-CTX-01). "
    "Use mcp__code-review-graph__ast_context_tool("
    f"file_path=..., {symbol_hint.strip() or 'no symbol'}) for signatures+callers, "
    "symbol_source_tool for one definition, query_graph(file_summary) for the node "
    "list, or Read with offset/limit for the slice you will edit."
)
print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse",
                                         "permissionDecision": "deny",
                                         "permissionDecisionReason": msg}}))
PY
