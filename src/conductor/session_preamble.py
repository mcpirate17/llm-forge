#!/usr/bin/env python3
"""Compact session inject shared by every coding agent.

Claude/Codex/Qwen SessionStart hooks can attach this text. Grok ignores
SessionStart stdout, so Grok law stays in ``AGENTS.md``; this module is
still the measured budget and the Claude-family inject.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any, Final

ROOT: Final[Path] = Path(__file__).resolve().parents[1]
ACTIVE_STATE_PATH: Final[Path] = ROOT / "conductor" / "active_state.json"
MAX_INJECT_CHARS: Final[int] = 2200
MAX_A2A_CHARS: Final[int] = 1200
MAX_HEADINGS: Final[int] = 4

MISSION: Final[str] = (
    "MISSION: Beat frontier models with novel non-QKV mechanisms. "
    "Never replace a novel lane with a softmax/QKV twin. Gate drops are defects."
)
RETRIEVE: Final[str] = (
    "RETRIEVE (do not dump .current_work.md): "
    '`python -m conductor.kb_retrieve query "<task>" --top-k 5` then '
    '`python -m conductor.memory_index query "<task>" --top-k 8`. '
    "Code: code-review-graph MCP provider=openai model=qwen3-embed-cpu. "
    "Status: `python -m conductor.handoff append` (max 12 lines). "
    "Findings: research/notes/ then `memory_index index`."
)
FLEET: Final[str] = (
    "FLEET: embed http://127.0.0.1:7317/v1 (GPU-guest, num_ctx=2048, unload). "
    "Clerk qwen3.5:9b GPU-always, clerical-only, zero approval authority; "
    "never gate work or runs on local output. Do not load 27B. "
    "Paired probes: --compile-mode default (KB-HW-01)."
)
MUTATION: Final[str] = (
    "MUTATION: new/changed tests need a registered campaign PASS receipt. "
    "`make mutation-coverage`. Never run mutants without Tim."
)


class PreambleError(ValueError):
    """Rejected session preamble."""


def load_state(
    path: Path = ACTIVE_STATE_PATH,
    *,
    refresh: bool | None = None,
) -> dict[str, Any]:
    """Load state, synchronously refreshing the canonical cache by default."""
    should_refresh = path == ACTIVE_STATE_PATH if refresh is None else refresh
    if should_refresh:
        try:
            from conductor.active_state import save_active_state

            return save_active_state(path).to_dict()
        except (OSError, RuntimeError, TypeError, ValueError) as exc:
            raise PreambleError(f"active-state refresh failed: {exc}") from exc
    if not path.is_file():
        raise PreambleError(f"active-state file does not exist: {path}")
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise PreambleError(f"active-state file is unreadable: {path}: {exc}") from exc
    if not isinstance(payload, dict):
        raise PreambleError(f"active-state payload must be an object: {path}")
    return payload


def compact_state(state: dict[str, Any]) -> str:
    mandates = state.get("standing_mandates")
    mandate_ids: list[str] = []
    if isinstance(mandates, list):
        for item in mandates:
            if not isinstance(item, str) or not item:
                continue
            mandate_ids.append(item.split(":", 1)[0].strip())
    headings = state.get("active_headings")
    heading_lines: list[str] = []
    if isinstance(headings, list):
        for item in headings[:MAX_HEADINGS]:
            if isinstance(item, str) and item.strip():
                heading_lines.append(f"- {item.strip()}")
    claims = state.get("active_claims")
    n_claims = len(claims) if isinstance(claims, list) else 0
    lines = [
        MISSION,
        RETRIEVE,
        FLEET,
        MUTATION,
        "MANDATES: " + (", ".join(mandate_ids) if mandate_ids else "none"),
        f"CLAIMS: {n_claims} active. Inspect with `make governance-claims`.",
    ]
    if heading_lines:
        lines.append("HEADINGS:")
        lines.extend(heading_lines)
    return "\n".join(lines)


def render_text(
    *,
    state: dict[str, Any] | None = None,
    a2a_name: str = "",
    a2a_summary: str = "",
    max_chars: int = MAX_INJECT_CHARS,
) -> str:
    body = compact_state(state if state is not None else load_state())
    name = a2a_name.strip()
    summary = a2a_summary.strip()
    if name and summary:
        clipped = summary[:MAX_A2A_CHARS]
        body += (
            f"\nA2A unread ({name}); ack: "
            f"`python -m conductor.agent_a2a read --as-name {name} <id>`\n"
            f"{clipped}"
        )
    if len(body) > max_chars:
        body = body[: max_chars - 1].rstrip() + "…"
    return body


def hook_payload(
    *,
    event_name: str = "SessionStart",
    state: dict[str, Any] | None = None,
    a2a_name: str = "",
    a2a_summary: str = "",
) -> dict[str, Any]:
    text = render_text(state=state, a2a_name=a2a_name, a2a_summary=a2a_summary)
    return {
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": text,
        }
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Compact agent session preamble")
    parser.add_argument("command", choices=["text", "hook"])
    parser.add_argument("--a2a-name", default=os.environ.get("A2A_AGENT_NAME", ""))
    parser.add_argument("--a2a-summary", default=os.environ.get("A2A_SUMMARY", ""))
    parser.add_argument("--state", type=Path, default=ACTIVE_STATE_PATH)
    args = parser.parse_args(argv)
    try:
        state = load_state(args.state)
    except PreambleError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2
    if args.command == "text":
        sys.stdout.write(
            render_text(
                state=state, a2a_name=args.a2a_name, a2a_summary=args.a2a_summary
            )
        )
        if not args.a2a_summary:
            sys.stdout.write("\n")
        return 0
    print(
        json.dumps(
            hook_payload(
                state=state,
                a2a_name=args.a2a_name,
                a2a_summary=args.a2a_summary,
            )
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
