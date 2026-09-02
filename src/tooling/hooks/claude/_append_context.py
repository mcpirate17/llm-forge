#!/usr/bin/env python3
"""Merge a project extension's output into a hook result's additionalContext.

Generic hooks under ``.claude/hooks`` carry no project knowledge; anything a
specific repo wants to say at SessionStart is produced by its project
extension (``.claude/hooks/project/<hook-name>``). This filter is the merge
step of that convention: it reads the generic hook's JSON result on stdin and
appends ``$PROJECT_HOOK_CONTEXT`` to ``hookSpecificOutput.additionalContext``,
creating the field when the generic result had none.

The merge is additive only. A malformed or empty ``PROJECT_HOOK_CONTEXT``, a
non-object payload, or unparseable stdin all pass the input through byte-for-
byte, so a broken project extension can never blank out or shrink the generic
context. stdlib only -- hooks run under bare ``python3``.
"""

from __future__ import annotations

import json
import os
import sys
from typing import Any, Final

ENV_VAR: Final[str] = "PROJECT_HOOK_CONTEXT"
SEPARATOR: Final[str] = "\n\n"


def merge(payload: Any, extra: str) -> Any:
    """Return *payload* with *extra* appended to its additionalContext."""
    if not extra.strip() or not isinstance(payload, dict):
        return payload
    out = dict(payload)
    specific = out.get("hookSpecificOutput")
    specific = dict(specific) if isinstance(specific, dict) else {}
    existing = specific.get("additionalContext")
    existing = existing if isinstance(existing, str) else ""
    specific["additionalContext"] = (
        f"{existing}{SEPARATOR}{extra.strip()}" if existing else extra.strip()
    )
    out["hookSpecificOutput"] = specific
    return out


def main() -> int:
    raw = sys.stdin.read()
    extra = os.environ.get(ENV_VAR, "")
    if not extra.strip():
        sys.stdout.write(raw)
        return 0
    try:
        payload = json.loads(raw)
    except (json.JSONDecodeError, ValueError):
        sys.stdout.write(raw)
        return 0
    print(json.dumps(merge(payload, extra)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
