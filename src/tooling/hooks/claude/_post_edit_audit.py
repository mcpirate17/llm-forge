#!/usr/bin/env python3
"""PostToolUse/Edit|Write: auto-format the edited file, then a structural audit.

Phase 1 formats deterministically and silently (``ruff`` for Python,
``rustfmt`` for Rust; a missing or failing formatter is not the hook's
failure). Phase 2 reports god files, god functions, commented-out code and
bare excepts as advisory ``additionalContext``. ``post-edit.sh`` runs this
body; the dispatcher calls ``hook_output`` directly.
"""

from __future__ import annotations

import ast
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any, Final

PROSE: Final[tuple[str, ...]] = (".md", ".txt", ".rst", ".jsonl", ".csv", ".json")
GOD_FILE_LINES: Final[int] = 1250
GOD_FUNCTION_LINES: Final[int] = 100
COMMENTED_CODE = re.compile(
    r"^\s*#\s*(def |class |import |from |return |raise |for |while )"
)
BARE_EXCEPT = re.compile(r"^\s*except\s*:")
QUIET: Final[dict[str, Any]] = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}


def _run_quiet(argv: list[str]) -> None:
    try:
        subprocess.run(
            argv,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=12,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return


def format_file(path: str) -> None:
    if path.endswith(".py"):
        _run_quiet(["ruff", "check", "--fix", "--quiet", path])
        _run_quiet(["ruff", "format", "--quiet", path])
    elif path.endswith(".rs"):
        _run_quiet(["rustfmt", "--edition", "2021", "--quiet", path])


def audit(path: str) -> list[str]:
    warnings: list[str] = []
    try:
        content = Path(path).read_text()
    except (OSError, UnicodeDecodeError):
        return warnings
    lines = content.splitlines()
    if len(lines) > GOD_FILE_LINES and not path.endswith(PROSE):
        warnings.append(f"{path}: {len(lines)} lines. Split this file.")
    if not path.endswith(".py"):
        return warnings
    try:
        tree = ast.parse(content)
    except SyntaxError:
        tree = None
    if tree is not None:
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                length = (node.end_lineno or node.lineno) - node.lineno + 1
                if length > GOD_FUNCTION_LINES:
                    warnings.append(
                        f"{node.name}() is {length} lines at line {node.lineno}. Break it up."
                    )
    commented = sum(1 for line in lines if COMMENTED_CODE.match(line))
    if commented > 2:
        warnings.append(f"{commented} lines of commented-out code. Delete them.")
    bare = sum(1 for line in lines if BARE_EXCEPT.match(line))
    if bare > 0:
        warnings.append(f"{bare} bare except clause(s). Catch specific exceptions.")
    return warnings


def hook_output(payload: Any) -> dict[str, Any]:
    tool_input = payload.get("tool_input") if isinstance(payload, dict) else None
    path = str((tool_input or {}).get("file_path") or "")
    if not path or not Path(path).is_file():
        return QUIET
    format_file(path)
    warnings = audit(path)
    if not warnings:
        return QUIET
    return {
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": "POST-EDIT AUDIT: " + " | ".join(warnings),
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
