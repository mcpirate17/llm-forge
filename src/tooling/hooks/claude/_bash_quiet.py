#!/usr/bin/env python3
"""Bound Bash output before the model sees it (PostToolUse/Bash).

``post-bash-quiet.sh`` feeds the hook payload to ``main()``. When the Bash
response's text fields exceed ``BASH_QUIET_LIMIT_BYTES`` (8 KB), the hook
returns the rewritten response under ``OUTPUT_FIELD`` = the same response
object with each long field replaced by head + elision marker + tail; the
full text is saved under ``$BASH_QUIET_SAVE_DIR`` (relative paths resolve
against the repo root), so nothing accumulates. This repo sets that variable
in ``.claude/hooks/project/env.sh``, which ``post-bash-quiet.sh`` sources when
present; with no project extension the fallback is a tempdir. The response
shape is preserved key-for-key because the harness validates that field
against the tool's output schema. Small outputs return no rewrite at all.

Claude Code and Codex share this script (``.claude/settings.json`` and
``.codex/hooks.json`` both point at ``post-bash-quiet.sh``) but their
``PostToolUseHookSpecificOutputWire`` structs use different field names for
the rewrite: Claude Code expects ``updatedToolOutput``, Codex's wire struct
only recognizes ``updatedMCPToolOutput`` and rejects the whole hook payload
as invalid when it sees the Claude-only key instead. ``.codex/hooks.json``
sets ``BASH_QUIET_OUTPUT_FIELD=updatedMCPToolOutput`` on its invocation to
select the Codex-compatible name; Claude Code's invocation leaves it unset
and gets the ``updatedToolOutput`` default.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Final

# The checkout served: the launcher/shell entry point passes PROJECT_DIR; run
# directly, the body sits at <root>/tooling/hooks/claude/.
REPO_ROOT: Final[Path] = Path(
    os.environ.get("PROJECT_DIR") or Path(__file__).resolve().parents[3]
).resolve()


def _save_dir() -> Path:
    """Where full outputs are parked: $BASH_QUIET_SAVE_DIR, else a tempdir.

    A relative value resolves against the repo root so a project extension can
    point at a repo scratch tree without knowing the checkout's location.
    """
    configured = os.environ.get("BASH_QUIET_SAVE_DIR", "").strip()
    if not configured:
        return Path(tempfile.gettempdir()) / "agent-bash-output"
    path = Path(configured).expanduser()
    return path if path.is_absolute() else REPO_ROOT / path


SAVE_DIR: Final[Path] = _save_dir()
LIMIT_BYTES: Final[int] = int(os.environ.get("BASH_QUIET_LIMIT_BYTES", "8000"))
OUTPUT_FIELD: Final[str] = os.environ.get(
    "BASH_QUIET_OUTPUT_FIELD", "updatedToolOutput"
)
HEAD_LINES: Final[int] = 60
TAIL_LINES: Final[int] = 30
TEXT_FIELDS: Final[tuple[str, ...]] = ("stdout", "stderr", "output")


def _save(data: bytes) -> Path:
    SAVE_DIR.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256(data).hexdigest()[:10]
    path = SAVE_DIR / f"{time.strftime('%Y%m%dT%H%M%S')}-{digest}.txt"
    path.write_bytes(data)
    return path


def bound(data: bytes) -> bytes:
    """Return *data* unchanged when small; otherwise head + marker + tail."""
    if len(data) <= LIMIT_BYTES:
        return data
    lines = data.splitlines(keepends=True)
    saved = _save(data)
    where = saved.relative_to(REPO_ROOT) if saved.is_relative_to(REPO_ROOT) else saved
    if len(lines) <= HEAD_LINES + TAIL_LINES:
        # Few but very long lines: cut by bytes instead.
        head, tail = data[: LIMIT_BYTES // 2], data[-(LIMIT_BYTES // 4) :]
        elided = len(data) - len(head) - len(tail)
        marker = f"\n... [elided {elided:,} bytes; full output: {where}] ...\n"
        return head + marker.encode() + tail
    head_lines, tail_lines = lines[:HEAD_LINES], lines[-TAIL_LINES:]
    elided = len(lines) - HEAD_LINES - TAIL_LINES
    marker = (
        f"... [elided {elided:,} lines / {len(data) // 1024} KB; "
        f"full output: {where}] ...\n"
    )
    return b"".join(head_lines) + marker.encode() + b"".join(tail_lines)


def bound_response(response: Any) -> Any | None:
    """Bounded copy of a Bash tool response, or None when nothing exceeds the limit."""
    if isinstance(response, str):
        out = bound(response.encode("utf-8", "surrogateescape"))
        return None if len(out) == len(response) else out.decode("utf-8", "replace")
    if not isinstance(response, dict):
        return None
    updated = dict(response)
    changed = False
    for key in TEXT_FIELDS:
        value = response.get(key)
        if isinstance(value, str) and len(value) > LIMIT_BYTES:
            updated[key] = bound(value.encode("utf-8", "surrogateescape")).decode(
                "utf-8", "replace"
            )
            changed = True
    return updated if changed else None


def hook_output(payload: Any) -> dict[str, Any]:
    out: dict[str, Any] = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    if not isinstance(payload, dict):
        return out
    updated = bound_response(payload.get("tool_response"))
    if updated is not None:
        out["hookSpecificOutput"][OUTPUT_FIELD] = updated
    return out


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        payload = None
    print(json.dumps(hook_output(payload)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
