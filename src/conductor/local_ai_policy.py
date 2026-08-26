#!/usr/bin/env python3
"""Fail-closed authority boundary for local generative models.

Local models are useful clerks, not reviewers or operators.  This module keeps
the trust decision independent from any provider name: a runtime-bound
``user`` or ``frontier_model`` authority may approve work where repository law
allows it, while ``local_model`` never may.
"""

from __future__ import annotations

import re
import shlex
from enum import StrEnum
from pathlib import Path
from typing import Final


class LocalAIPolicyError(ValueError):
    """A local-model request crossed the clerical-only trust boundary."""


class ApprovalAuthority(StrEnum):
    """Provider-agnostic authority classes bound by the calling runtime."""

    USER = "user"
    FRONTIER_MODEL = "frontier_model"
    LOCAL_MODEL = "local_model"


ALLOWED_LOCAL_TASKS: Final[frozenset[str]] = frozenset(
    {"notes", "summary", "organization", "compaction"}
)
CLERK_SYSTEM_PROMPT: Final[str] = (
    "You are a low-risk local workspace clerk. You may only help with notes, "
    "summaries, organization, and compaction. You have zero authority to approve, "
    "authorize, sign off, promote, launch, resume, continue, or otherwise permit "
    "work, experiments, evaluations, optimizer steps, GPU use, or training runs. "
    "Never provide a go/no-go or approval verdict. If asked for one, refuse and say "
    "that Tim or a runtime-verified frontier model is required; multi-hour runs "
    "still require Tim's explicit approval."
)
DENY_REASON: Final[str] = (
    "BLOCKED: local AI is clerical-only and has zero approval authority. Local "
    "models may handle notes, summaries, organization, or compaction, but may never "
    "approve, authorize, sign off, promote, launch, resume, or continue work or "
    "runs. Use Tim or a runtime-verified frontier model where policy permits; "
    "multi-hour training still requires Tim's explicit approval."
)
UNCLASSIFIED_REASON: Final[str] = (
    "BLOCKED: local generative inference requires an explicit clerical task class. "
    "Set LOCAL_AI_TASK to notes, summary, organization, or compaction. Local AI "
    "cannot be used for approval or run decisions."
)

_SHELL_OPERATORS: Final[frozenset[str]] = frozenset({";", "&", "&&", "|", "||"})
_SHELL_RUNNERS: Final[frozenset[str]] = frozenset({"bash", "dash", "ksh", "sh", "zsh"})
_LOCAL_CHAT_ENDPOINT_RE: Final[re.Pattern[str]] = re.compile(
    r"https?://(?:127\.0\.0\.1|localhost|\[::1\])(?::11434)?/api/(?:chat|generate)\b",
    flags=re.IGNORECASE,
)
_TASK_ASSIGNMENT_RE: Final[re.Pattern[str]] = re.compile(
    r"(?:^|[\s;])(?:export\s+)?LOCAL_AI_TASK=([A-Za-z_-]+)(?=$|[\s;])"
)
_AUTHORITY_PATTERNS: Final[tuple[re.Pattern[str], ...]] = tuple(
    re.compile(pattern, flags=re.IGNORECASE | re.DOTALL)
    for pattern in (
        r"\bapprov(?:e|al|ed|er|ing)\b",
        r"\bauthori[sz](?:e|ed|ation|ing)\b",
        r"\bpermission\b",
        r"\bsign(?:ed|ing)?[- ]?off\b",
        r"\bgreen[- ]?light\b",
        r"\bgo\s*[/_-]?\s*no[- ]?go\b",
        r"\bfinal\s+verdict\b",
        r"\b(?:should|may|can)\s+(?:i|we|the\s+agent)\b",
        r"\b(?:decide|recommend|determine)\s+(?:whether|if)\b",
        r"\breturn\b.{0,40}\b(?:pass|not[_ -]?ready|fail[- ]?closed)\b",
        r"\b(?:launch|relaunch|resume|continue|start|conduct|promote)\w*\b"
        r".{0,50}\b(?:run|training|experiment|optimizer|smoke)\w*\b",
        r"\b(?:run|training|experiment|optimizer|smoke)\w*\b"
        r".{0,50}\b(?:launch|relaunch|resume|continue|start|conduct|promote)\w*\b",
    )
)


def approval_authority_allowed(authority: str | ApprovalAuthority) -> bool:
    """Return whether a runtime-bound authority class may approve work."""

    try:
        normalized = ApprovalAuthority(authority)
    except ValueError:
        return False
    return normalized in {ApprovalAuthority.USER, ApprovalAuthority.FRONTIER_MODEL}


def prompt_requests_authority(prompt: str) -> bool:
    """Detect approval or operational-decision requests in a local prompt."""

    return any(pattern.search(prompt) for pattern in _AUTHORITY_PATTERNS)


def require_clerical_task(task_class: str, prompt: str) -> str:
    """Validate a local task and return its normalized clerical class."""

    normalized = task_class.strip().casefold().replace("-", "_")
    if normalized not in ALLOWED_LOCAL_TASKS:
        expected = ", ".join(sorted(ALLOWED_LOCAL_TASKS))
        raise LocalAIPolicyError(
            f"local task class {task_class!r} is not clerical; expected one of {expected}"
        )
    if prompt_requests_authority(prompt):
        raise LocalAIPolicyError(DENY_REASON)
    return normalized


def _command_segments(command: str) -> list[list[str]] | None:
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|")
        lexer.whitespace_split = True
        lexer.commenters = ""
        tokens = list(lexer)
    except ValueError:
        return None
    segments: list[list[str]] = [[]]
    for token in tokens:
        if token in _SHELL_OPERATORS:
            if segments[-1]:
                segments.append([])
            continue
        segments[-1].append(token)
    return [segment for segment in segments if segment]


def _command_argv(segment: list[str]) -> tuple[str, list[str]]:
    index = 0
    while index < len(segment) and re.match(
        r"^[A-Za-z_][A-Za-z0-9_]*=", segment[index]
    ):
        index += 1
    if index < len(segment) and Path(segment[index]).name == "env":
        index += 1
        while index < len(segment):
            token = segment[index]
            if token.startswith("-") or re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", token):
                index += 1
                continue
            break
    if index >= len(segment):
        return "", []
    return Path(segment[index]).name, segment[index + 1 :]


def _is_local_chat(segment: list[str]) -> bool:
    executable, arguments = _command_argv(segment)
    if executable in _SHELL_RUNNERS and "-c" in arguments:
        index = arguments.index("-c")
        if index + 1 < len(arguments):
            return deny_local_ai_command(arguments[index + 1]) is not None
    if executable == "ollama" and arguments and arguments[0] == "run":
        return True
    return executable not in {"echo", "printf", "rg", "grep"} and any(
        _LOCAL_CHAT_ENDPOINT_RE.search(token) for token in segment
    )


def _task_class(command: str) -> str | None:
    match = _TASK_ASSIGNMENT_RE.search(command)
    return match.group(1) if match else None


def deny_local_ai_command(
    command: str,
    *,
    local_runtime: bool = False,
) -> str | None:
    """Return a hook denial for unsafe local inference, otherwise ``None``."""

    if local_runtime and prompt_requests_authority(command):
        return DENY_REASON
    segments = _command_segments(command)
    if segments is None:
        if re.search(r"\bollama\s+run\b", command) or _LOCAL_CHAT_ENDPOINT_RE.search(
            command
        ):
            return UNCLASSIFIED_REASON
        return None
    local_segments = [segment for segment in segments if _is_local_chat(segment)]
    if not local_segments:
        return None
    task_class = _task_class(command)
    if task_class is None:
        return UNCLASSIFIED_REASON
    try:
        require_clerical_task(task_class, command)
    except LocalAIPolicyError as exc:
        return str(exc)
    return None
