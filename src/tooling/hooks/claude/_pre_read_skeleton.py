#!/usr/bin/env python3
"""PreToolUse/Read: deny a whole-file Read of a code file >= 400 lines.

The KB-OPS-CTX-01 rule, with a reason that names the bounded alternatives: the
graph's AST projection (``mcp__code-review-graph__ast_context_tool``),
``symbol_source_tool``, or Read with offset/limit. Small files, non-code files
and sliced reads pass untouched. ``pre-read-skeleton.sh`` runs this body; the
dispatcher calls ``hook_output`` directly.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any, Final

THRESHOLD_LINES: Final[int] = 400
CODE_SUFFIXES: Final[frozenset[str]] = frozenset(
    {".py", ".rs", ".c", ".cc", ".cpp", ".h", ".hpp", ".ts", ".js", ".sh"}
)
QUIET: Final[dict[str, Any]] = {"hookSpecificOutput": {"hookEventName": "PreToolUse"}}


def hook_output(payload: Any) -> dict[str, Any]:
    if not isinstance(payload, dict):
        return QUIET
    tool_input = payload.get("tool_input") or payload.get("toolInput") or {}
    path = tool_input.get("file_path") or tool_input.get("path") or ""
    if not path or tool_input.get("offset") or tool_input.get("limit"):
        return QUIET
    file = Path(path)
    if file.suffix not in CODE_SUFFIXES or not file.is_file():
        return QUIET
    try:
        data = file.read_bytes()
    except OSError:
        return QUIET
    lines = data.count(b"\n")
    if lines < THRESHOLD_LINES:
        return QUIET
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
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": msg,
        }
    }


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        payload = None
    out = hook_output(payload)
    # The quiet response is emitted compact, as the shell hooks always did.
    print(json.dumps(out, separators=(",", ":") if out is QUIET else None))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
