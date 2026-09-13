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

from conductor.session_policy import SessionPolicyError, load_session_policy

from conductor.project_paths import host_root
ROOT: Final[Path] = host_root()
ACTIVE_STATE_PATH: Final[Path] = ROOT / "conductor" / "active_state.json"
MAX_INJECT_CHARS: Final[int] = 2200
MAX_A2A_CHARS: Final[int] = 1200
MAX_HEADINGS: Final[int] = 4


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


def compact_state(state: dict[str, Any], *, repo: Path = ROOT) -> str:
    """The preamble text: policy lines, then the live-state summary.

    The EXPOSED line used to be spliced in here; it moved to its own registry
    hook (``workspace_exposure_session`` -> ``workspace_hygiene.exposure_line``)
    so the dispatcher can serve it natively without re-running the whole
    preamble, and so its degrade path cannot take the inject down with it.
    """
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
        *load_session_policy(repo).preamble,
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
    repo: Path = ROOT,
) -> str:
    selected_state = state
    if selected_state is None:
        selected_state = load_state(repo / "conductor" / "active_state.json")
    body = compact_state(selected_state, repo=repo)
    name = a2a_name.strip()
    summary = a2a_summary.strip()
    if name and summary:
        clipped = summary[:MAX_A2A_CHARS]
        body += (
            f"\nA2A compact ({name}); retrieve only when needed: "
            f"`python -m conductor.agent_a2a show --as-name {name} <id>`; ack: "
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
    repo: Path = ROOT,
) -> dict[str, Any]:
    text = render_text(
        state=state, a2a_name=a2a_name, a2a_summary=a2a_summary, repo=repo
    )
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
    parser.add_argument("--state", type=Path)
    parser.add_argument("--repo", type=Path)
    args = parser.parse_args(argv)
    if args.state is None:
        repo = args.repo or ROOT
        state_path = repo / "conductor" / "active_state.json"
        refresh = None
    else:
        state_path = args.state
        refresh = None
        inferred_repo = (
            state_path.parent.parent
            if state_path.name == "active_state.json"
            and state_path.parent.name == "conductor"
            else None
        )
        if args.repo is None:
            if inferred_repo is None:
                parser.error(
                    "--repo is required when --state is not <repo>/conductor/active_state.json"
                )
            repo = inferred_repo
        else:
            repo = args.repo
            if inferred_repo is not None and repo.resolve() != inferred_repo.resolve():
                parser.error(
                    "--repo conflicts with the repository implied by canonical --state"
                )
    try:
        state = load_state(state_path, refresh=refresh)
        if args.command == "text":
            sys.stdout.write(
                render_text(
                    state=state,
                    a2a_name=args.a2a_name,
                    a2a_summary=args.a2a_summary,
                    repo=repo,
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
                    repo=repo,
                )
            )
        )
        return 0
    except (PreambleError, SessionPolicyError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
