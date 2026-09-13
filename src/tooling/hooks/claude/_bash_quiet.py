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
from typing import Any, Callable, Final

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


def split_head_tail(
    data: bytes, limit_bytes: int
) -> tuple[bytes, bytes, int, int]:
    """Head and tail halves of the bounding split, with what they elide.

    Line-count split first (``HEAD_LINES`` + ``TAIL_LINES``); a response of
    few but very long lines falls back to a byte split. Returns
    ``(head, tail, elided_bytes, elided_lines)`` so a caller can compose its
    own elision marker -- Bash names a spill path, Read names a resume
    offset -- without duplicating the split.
    """
    lines = data.splitlines(keepends=True)
    if len(lines) <= HEAD_LINES + TAIL_LINES:
        # Few but very long lines: cut by bytes instead.
        head, tail = data[: limit_bytes // 2], data[-(limit_bytes // 4) :]
        return head, tail, len(data) - len(head) - len(tail), 0
    head, tail = b"".join(lines[:HEAD_LINES]), b"".join(lines[-TAIL_LINES:])
    return (
        head,
        tail,
        len(data) - len(head) - len(tail),
        len(lines) - HEAD_LINES - TAIL_LINES,
    )


def bound(data: bytes, *, limit_bytes: int | None = None) -> bytes:
    """Return *data* unchanged when small; otherwise head + marker + tail."""
    limit = LIMIT_BYTES if limit_bytes is None else limit_bytes
    if len(data) <= limit:
        return data
    saved = _save(data)
    where = saved.relative_to(REPO_ROOT) if saved.is_relative_to(REPO_ROOT) else saved
    head, tail, elided_bytes, elided_lines = split_head_tail(data, limit)
    if elided_lines == 0:
        marker = f"\n... [elided {elided_bytes:,} bytes; full output: {where}] ...\n"
    else:
        marker = (
            f"... [elided {elided_lines:,} lines / {len(data) // 1024} KB; "
            f"full output: {where}] ...\n"
        )
    return head + marker.encode() + tail


def bound_response(response: Any) -> Any | None:
    """Bounded copy of a Bash tool response, or None when nothing exceeds the limit."""
    if isinstance(response, str):
        data = response.encode("utf-8", "surrogateescape")
        out = bound(data)
        return None if out == data else out.decode("utf-8", "replace")
    if not isinstance(response, dict):
        return None
    updated = dict(response)
    changed = False
    for key in TEXT_FIELDS:
        value = response.get(key)
        # The declared bound is 8 KB of OUTPUT, not 8000 characters: a multi-byte
        # response between 8000 characters and 8000 bytes used to slip through
        # unbounded because the gate counted characters.
        if (
            isinstance(value, str)
            and len(value.encode("utf-8", "surrogateescape")) > LIMIT_BYTES
        ):
            updated[key] = bound(value.encode("utf-8", "surrogateescape")).decode(
                "utf-8", "replace"
            )
            changed = True
    return updated if changed else None


def rewrite_envelope(
    payload: Any, bound_response: Callable[[Any], Any | None]
) -> dict[str, Any]:
    """The shared PostToolUse envelope every quiet hook returns.

    Each hook passes its own ``bound_response`` (Bash bounds the
    stdout/stderr/output fields, the tool hook bounds Read/Grep/MCP text);
    the rewrite lands under the host's ``OUTPUT_FIELD``.
    """
    out: dict[str, Any] = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    if not isinstance(payload, dict):
        return out
    updated = bound_response(payload.get("tool_response"))
    if updated is not None:
        out["hookSpecificOutput"][OUTPUT_FIELD] = updated
    return out


def hook_output(payload: Any) -> dict[str, Any]:
    return rewrite_envelope(payload, bound_response)


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        payload = None
    print(json.dumps(hook_output(payload)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
