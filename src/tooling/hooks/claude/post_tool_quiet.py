#!/usr/bin/env python3
"""Bound Read, Grep and MCP tool output before the model sees it (PostToolUse).

``_bash_quiet`` bounds Bash; this is the same contract for everything else
the dispatcher sees on PostToolUse. When a tool response's text exceeds
``TOOL_OUTPUT_QUIET_BYTES`` (16 KB by default), the hook returns the
rewritten response under ``hookSpecificOutput.updatedToolOutput`` with the
long text replaced by head + elision marker + tail -- the split and the
spill directory are ``_bash_quiet``'s, reused by import.

- Read (``{"type": "text", "file": {"filePath", "content", ...}}``): only
  ``file.content`` is bounded, and nothing is spilled -- the file is already
  on disk. The marker names the byte where the cut happened and the
  ``Read(offset=..., limit=...)`` call that reaches the rest.
- Grep (plain result string) and MCP (string response or a list of
  ``{"type": "text", "text": ...}`` blocks): the full text is spilled to
  ``$BASH_QUIET_SAVE_DIR`` exactly like Bash output and the marker names the
  spill path.

``TOOL_OUTPUT_QUIET_BYTES=0`` disables the hook. Below the cap the response
passes through byte-identical -- no rewrite at all. An unrecognized response
shape is reported on stderr and passed through unchanged: a bounding hook
must never eat a response it does not understand, and never raise inside the
hook path.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any, Final

# Hook bodies are file-loaded by the dispatcher (no package context) and
# stem-imported by the tests beside them. Resolve _bash_quiet the same way so
# every context shares one instance: a SAVE_DIR patched for a test (or by a
# project env) must govern this hook's spills too.
_HOOKS_DIR: Final[Path] = Path(__file__).resolve().parent
if str(_HOOKS_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOKS_DIR))
import _bash_quiet as _bq  # noqa: E402

CAP_DEFAULT_BYTES: Final[int] = 16000


def _cap() -> int:
    """``$TOOL_OUTPUT_QUIET_BYTES`` (default 16 KB); ``0`` disables bounding."""
    return int(os.environ.get("TOOL_OUTPUT_QUIET_BYTES", str(CAP_DEFAULT_BYTES)))


def _encode(text: str) -> bytes:
    return text.encode("utf-8", "surrogateescape")


def _bounded_with_spill(text: str, cap: int) -> str:
    """The Bash treatment: head + marker naming the spill path + tail."""
    return _bq.bound(_encode(text), limit_bytes=cap).decode("utf-8", "replace")


def _bounded_read(content: str, cap: int) -> str:
    """Bounded ``file.content``: the pointer reaches the file, not a spill."""
    head, tail, elided, _ = _bq.split_head_tail(_encode(content), cap)
    cut = len(head)
    resume_line = head.count(b"\n") + 1
    marker = (
        f"\n... [elided {elided:,} bytes at byte {cut:,} (line {resume_line:,}); "
        f"read the rest with Read(offset={resume_line}, limit=...)] ...\n"
    )
    return head + marker.encode() + tail


def _warn_unrecognized(response: Any) -> None:
    kind = type(response).__name__
    print(
        f"post-tool-quiet: unrecognized tool_response shape ({kind}); "
        "passing through unbounded",
        file=sys.stderr,
    )


def bound_response(response: Any) -> Any | None:
    """Bounded copy of a Read/Grep/MCP response, or None when nothing to do."""
    cap = _cap()
    if cap <= 0:
        return None
    if isinstance(response, str):
        if len(_encode(response)) <= cap:
            return None
        return _bounded_with_spill(response, cap)
    if isinstance(response, dict):
        file_field = response.get("file")
        if isinstance(file_field, dict) and isinstance(file_field.get("content"), str):
            if response.get("type") not in (None, "text"):
                _warn_unrecognized(response)
                return None
            if len(_encode(file_field["content"])) <= cap:
                return None
            return {
                **response,
                "file": {
                    **file_field,
                    "content": _bounded_read(file_field["content"], cap).decode(
                        "utf-8", "replace"
                    ),
                },
            }
        text = response.get("text")
        if isinstance(text, str):
            if len(_encode(text)) <= cap:
                return None
            return {**response, "text": _bounded_with_spill(text, cap)}
        _warn_unrecognized(response)
        return None
    if isinstance(response, list):
        if not all(
            isinstance(block, dict) and isinstance(block.get("text"), str)
            for block in response
        ):
            _warn_unrecognized(response)
            return None
        updated = []
        changed = False
        for block in response:
            if len(_encode(block["text"])) > cap:
                updated.append(
                    {**block, "text": _bounded_with_spill(block["text"], cap)}
                )
                changed = True
            else:
                updated.append(block)
        return updated if changed else None
    _warn_unrecognized(response)
    return None


def hook_output(payload: Any) -> dict[str, Any]:
    out: dict[str, Any] = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    if not isinstance(payload, dict):
        return out
    updated = bound_response(payload.get("tool_response"))
    if updated is not None:
        out["hookSpecificOutput"][_bq.OUTPUT_FIELD] = updated
    return out


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        payload = None
    try:
        print(json.dumps(hook_output(payload)))
    except (TypeError, ValueError) as exc:
        print(f"post-tool-quiet: cannot serialize rewrite: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
