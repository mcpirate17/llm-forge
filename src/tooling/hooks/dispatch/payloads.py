"""Synthetic hook payloads for the doctor and the latency bench.

Every payload is side-effect bounded: file paths point into *scratch*, the
session id is unique to the run, and ``scoped_env`` redirects every state the
hooks write (gate state, telemetry log, bash-output park) into *scratch*.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

SESSION_PREFIX = "hook-doctor-"


def scoped_env(project_dir: Path, scratch: Path) -> dict[str, str]:
    env = dict(os.environ)
    env.update(
        {
            "CLAUDE_PROJECT_DIR": str(project_dir),
            "CRG_GATE_STATE_DIR": str(scratch / "crg-gate"),
            "CONTEXT_TELEMETRY_PATH": str(scratch / "events.jsonl"),
            "BASH_QUIET_SAVE_DIR": str(scratch / "bash-output"),
            "CRG_SKIP_EMBED": "1",
        }
    )
    return env


def _scratch_file(scratch: Path, name: str, text: str) -> str:
    path = scratch / name
    path.write_text(text, encoding="utf-8")
    return str(path)


def synthetic(
    event: str, tool: str, project_dir: Path, scratch: Path
) -> dict[str, Any]:
    scratch.mkdir(parents=True, exist_ok=True)
    session = f"{SESSION_PREFIX}{os.getpid()}"
    base: dict[str, Any] = {
        "session_id": session,
        "transcript_path": str(scratch / "transcript.jsonl"),
        "cwd": str(project_dir),
        "hook_event_name": event,
    }
    if event == "SessionStart":
        return {**base, "source": "startup"}
    if event == "SessionEnd":
        # No ``reason``: obsidian_sync session-end exits without writing when the
        # accumulator is empty and the reason is blank.
        return base
    base["tool_name"] = tool
    if tool == "Bash":
        base["tool_input"] = {"command": "echo hook-doctor", "description": "probe"}
        response: Any = {"stdout": "hook-doctor\n", "stderr": "", "interrupted": False}
    elif tool == "Read":
        path = _scratch_file(scratch, "probe_read.py", "x = 1\n")
        base["tool_input"] = {"file_path": path}
        response = {"type": "text", "file": {"filePath": path, "content": "x = 1\n"}}
    elif tool in ("Edit", "Write", "NotebookEdit"):
        path = _scratch_file(scratch, "probe_edit.py", "x = 1\n")
        base["tool_input"] = {"file_path": path, "old_string": "x", "new_string": "y"}
        response = {"filePath": path, "success": True}
    elif tool.startswith("mcp__"):
        base["tool_input"] = {"task": "probe"}
        response = {"result": "ok"}
    else:
        raise ValueError(f"no synthetic payload for tool {tool!r}")
    if event == "PostToolUse":
        base["tool_response"] = response
    return base


TOOLS_PER_EVENT: dict[str, tuple[str, ...]] = {
    "PreToolUse": ("Bash", "Read", "Edit", "mcp__code-review-graph__locate_tool"),
    "PostToolUse": ("Bash", "Read", "Edit"),
    "SessionStart": ("",),
    "SessionEnd": ("",),
}
