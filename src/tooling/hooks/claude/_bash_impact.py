#!/usr/bin/env python3
"""Legacy Bash impact entrypoint backed by forge's native classifier."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from native_legacy_bridge import call


def _classify(command: str) -> tuple[str, str]:
    tier, detail = call("impact-classify", command=command)
    return tier, detail


def main() -> None:
    try:
        payload: Any = json.load(sys.stdin)
    except json.JSONDecodeError:
        payload = {}
    command = ""
    if isinstance(payload, dict):
        tool_input = payload.get("tool_input")
        if isinstance(tool_input, dict):
            command = str(tool_input.get("command") or "")
    additional = call("impact-context", command=command) if command else None
    output: dict[str, Any] = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
        }
    }
    if additional:
        output["hookSpecificOutput"]["additionalContext"] = additional
    print(json.dumps(output))


if __name__ == "__main__":
    main()
