#!/usr/bin/env python3
"""Cooperative PreToolUse gates for workspace coordination and local AI.

Agents must use retrieval and bounded conductor utilities instead of reading or
editing ``.current_work.md`` directly. Local generative models are clerical-only
and may never approve or authorize work. These are tool-policy boundaries, not
an OS sandbox: arbitrary code can still bypass cooperative hooks.
"""

from __future__ import annotations

import json
import os
import shlex
import sys
from pathlib import Path
from typing import Any, Final

from conductor.local_ai_policy import deny_local_ai_command

CURRENT_WORK_NAME: Final[str] = ".current_work.md"
MUTATION_HINT: Final[str] = (
    "Mutation evidence covers ONLY the files you changed and the tests that "
    "exercise them -- never the whole repo. `make mutation-plan`, then "
    "`make mutation-generate`, then `make mutation-engine-run`. Automatic "
    "engines only; hand-authored mutants, manifests, survivor baselines and "
    "receipts are forbidden (KB-MUT-02). A changed test with no current "
    "receipt is debt to record in the PR body, not a reason to hold the branch."
)
TEST_SUFFIXES: Final[tuple[str, ...]] = (
    "_test.py",
    "_test.c",
    "_test.cc",
    "_test.cpp",
    "_test.cxx",
    ".test.js",
    ".test.jsx",
    ".test.ts",
    ".test.tsx",
    ".spec.js",
    ".spec.jsx",
    ".spec.ts",
    ".spec.tsx",
    "Test.java",
)
DENY_HINT: Final[str] = (
    "Use `python -m conductor.kb_retrieve query ...` and "
    "`python -m conductor.memory_index query ...` to read workspace context. "
    "Write findings to research/notes/<topic>.md, then "
    "`python -m conductor.memory_index index`. Short status: "
    "`python -m conductor.handoff append --owner <you> --title '...' --body '...'` "
    "(max 12 lines)."
)
_SHELL_OPERATORS: Final[frozenset[str]] = frozenset({";", "&", "&&", "|", "||"})
_HARMLESS_MENTION_COMMANDS: Final[frozenset[str]] = frozenset({"echo", "printf"})
_SEARCH_COMMANDS: Final[frozenset[str]] = frozenset({"grep", "rg"})
_SHELL_TOOLS: Final[frozenset[str]] = frozenset(
    {"bash", "shell", "run_shell_command", "exec_command"}
)


def is_test_creation_path(path: str) -> bool:
    if not path:
        return False
    target = Path(path)
    name = target.name
    return (
        "test" in target.parts
        or name.startswith("test_")
        or name.endswith(TEST_SUFFIXES)
    )


def is_current_work_path(path: str) -> bool:
    if not path:
        return False
    return Path(path).name == CURRENT_WORK_NAME


def _tool_input(payload: dict[str, Any]) -> dict[str, Any]:
    raw = (
        payload.get("tool_input")
        or payload.get("toolInput")
        or payload.get("arguments")
        or payload.get("input")
    )
    return raw if isinstance(raw, dict) else {}


def _file_path(tool_input: dict[str, Any]) -> str:
    for key in ("file_path", "path", "filePath", "target_file"):
        value = tool_input.get(key)
        if isinstance(value, str) and value:
            return value
    return ""


def _tool_name(payload: dict[str, Any]) -> str:
    value = payload.get("tool_name") or payload.get("toolName")
    return value if isinstance(value, str) else ""


def _shell_command(tool_input: dict[str, Any]) -> str:
    value = tool_input.get("command") or tool_input.get("cmd")
    return value if isinstance(value, str) else ""


def _command_segments(command: str) -> list[list[str]]:
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|")
        lexer.whitespace_split = True
        lexer.commenters = ""
        tokens = list(lexer)
    except ValueError:
        return []
    segments: list[list[str]] = [[]]
    for token in tokens:
        if token in _SHELL_OPERATORS:
            if segments[-1]:
                segments.append([])
            continue
        segments[-1].append(token)
    return [segment for segment in segments if segment]


def _path_token(token: str) -> bool:
    return Path(token.rstrip("/")).name == CURRENT_WORK_NAME


def _first_positional(tokens: list[str]) -> int | None:
    for index, token in enumerate(tokens):
        if token == "--":
            return index + 1 if index + 1 < len(tokens) else None
        if not token.startswith("-"):
            return index
    return None


def _segment_reads_raw_log(segment: list[str]) -> bool:
    while segment and ("=" in segment[0] and not segment[0].startswith(("/", "./"))):
        segment = segment[1:]
    if segment and Path(segment[0]).name in {"env", "sudo"}:
        segment = segment[1:]
    if not segment:
        return False
    executable = Path(segment[0]).name
    arguments = segment[1:]
    target_indices = [
        index for index, token in enumerate(arguments) if _path_token(token)
    ]
    if not target_indices or executable in _HARMLESS_MENTION_COMMANDS:
        return False
    if executable in _SEARCH_COMMANDS:
        pattern_index = _first_positional(arguments)
        return pattern_index is not None and any(
            index > pattern_index for index in target_indices
        )
    return True


def deny_shell_access(command: str) -> str | None:
    """Deny recognizable shell reads/writes while allowing textual mentions."""
    if CURRENT_WORK_NAME not in command:
        return None
    if any(_segment_reads_raw_log(segment) for segment in _command_segments(command)):
        return f"BLOCKED: direct shell access to {CURRENT_WORK_NAME} is unsupported. {DENY_HINT}"
    return None


def evaluate_payload(payload: dict[str, Any]) -> str | None:
    tool_input = _tool_input(payload)
    path = _file_path(tool_input)
    if is_current_work_path(path):
        tool_name = _tool_name(payload) or "file tool"
        return (
            f"BLOCKED: {tool_name} cannot access {CURRENT_WORK_NAME} directly. "
            f"{DENY_HINT}"
        )
    tool_name = _tool_name(payload).casefold()
    if tool_name in _SHELL_TOOLS:
        command = _shell_command(tool_input)
        local_runtime = os.environ.get("LOCAL_AI_RUNTIME", "").casefold() in {
            "1",
            "true",
            "yes",
        }
        return deny_local_ai_command(
            command, local_runtime=local_runtime
        ) or deny_shell_access(command)
    return None


def advisory_for_payload(payload: dict[str, Any]) -> str | None:
    """Remind agents that new tests still need mutation-campaign evidence."""

    if evaluate_payload(payload) is not None:
        return None
    path = _file_path(_tool_input(payload))
    if is_test_creation_path(path):
        return MUTATION_HINT
    return None


def hook_protocol(payload: dict[str, Any]) -> str:
    """Identify Grok's camel-case protocol; Codex/Qwen/Claude use the other shape."""
    event_name = payload.get("hookEventName")
    if "toolName" in payload or event_name == "pre_tool_use":
        return "grok"
    return "codex"


def hook_response(
    reason: str | None,
    *,
    protocol: str = "codex",
    advisory: str | None = None,
) -> dict[str, Any] | None:
    """Return a protocol-correct deny or advisory response."""
    if reason is not None:
        if protocol == "grok":
            return {"decision": "deny", "reason": reason}
        if protocol != "codex":
            raise ValueError(f"unsupported hook protocol: {protocol!r}")
        return {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            },
        }
    if not advisory:
        return None
    if protocol == "grok":
        return {"hookSpecificOutput": {"additionalContext": advisory}}
    if protocol != "codex":
        raise ValueError(f"unsupported hook protocol: {protocol!r}")
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": advisory,
        },
    }


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except json.JSONDecodeError:
        payload = {}
    if not isinstance(payload, dict):
        payload = {}
    protocol = hook_protocol(payload)
    response = hook_response(
        evaluate_payload(payload),
        protocol=protocol,
        advisory=advisory_for_payload(payload),
    )
    if response is not None:
        print(json.dumps(response))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
