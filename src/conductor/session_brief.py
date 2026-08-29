#!/usr/bin/env python3
"""One-call session bootstrap: mandates, live headings, claims on *your* paths,
A2A previews, and the top knowledge cards for the task.

Replaces the 5–6 boilerplate calls every agent makes at session start
(active_state, kb_retrieve, memory_index, inbox, peers, claims) with one
~2 KB payload. Available as a CLI (``python -m conductor.session_brief brief``)
and as the ``session_brief_tool`` MCP tool. ``a2a-compact`` is the inbox
preview filter used by the session-start hook.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Final

from conductor import kb_retrieve
from conductor.candidate_review.cli import compact_claims_text
from conductor.candidate_review.ownership import (
    OwnershipError,
    load_claims,
    paths_overlap,
)
from conductor.session_preamble import MAX_HEADINGS, load_state

ROOT: Final[Path] = Path(__file__).resolve().parents[1]
MAX_MSGS: Final[int] = 8
PREVIEW_CHARS: Final[int] = 140
SNIPPET_CHARS: Final[int] = 240
CARDS_K: Final[int] = 3
MAX_BRIEF_CHARS: Final[int] = 3500


def snippet(text: str, limit: int = SNIPPET_CHARS) -> str:
    """Frontmatter-free, whitespace-collapsed preview of a note or card."""
    body = text
    if body.startswith("---"):
        end = body.find("\n---", 3)
        if end != -1:
            body = body[end + 4 :]
    body = " ".join(body.split())
    return body if len(body) <= limit else body[: limit - 1].rstrip() + "…"


def compact_inbox(
    text: str, *, max_msgs: int = MAX_MSGS, preview: int = PREVIEW_CHARS
) -> str:
    """Header + one-line preview per ``[UNREAD]`` message; never whole bodies."""
    msgs: list[str] = []
    header: str | None = None
    body: list[str] = []

    def flush() -> None:
        if header is None:
            return
        joined = " ".join(" ".join(body).split())
        if len(joined) > preview:
            joined = joined[: preview - 1] + "…"
        msgs.append(header + (f"\n  {joined}" if joined else ""))

    for line in text.splitlines():
        if line.startswith("[UNREAD]"):
            flush()
            header, body = line, []
        elif header is not None:
            body.append(line)
    flush()
    shown = msgs[:max_msgs]
    if len(msgs) > max_msgs:
        shown.append(f"(+{len(msgs) - max_msgs} more unread)")
    return "\n".join(shown)


def claims_for_paths(
    paths: list[str], *, repo: Path = ROOT, now: datetime | None = None
) -> str:
    if not paths:
        return "CLAIMS: no paths given"
    try:
        claims, digest = load_claims(repo)
    except (OSError, OwnershipError) as exc:
        return f"CLAIMS: unavailable ({exc})"
    now = now or datetime.now(timezone.utc)
    hits = [
        claim
        for claim in claims
        if claim.active(now)
        and any(paths_overlap(p, c) for p in paths for c in claim.paths)
    ]
    if not hits:
        return f"CLAIMS: none overlap {' '.join(paths)} — claim before editing"
    return "CLAIMS overlapping your paths:\n" + compact_claims_text(
        hits, digest, now=now, with_paths=True
    )


def inbox_preview(agent: str | None) -> str:
    if not agent:
        return ""
    try:
        proc = subprocess.run(
            [
                sys.executable,
                "-m",
                "conductor.agent_a2a",
                "inbox",
                "--as-name",
                agent,
                "--unread",
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return f"A2A: inbox unavailable ({exc})"
    if proc.returncode != 0:
        return f"A2A: inbox unavailable (exit {proc.returncode})"
    compact = compact_inbox(proc.stdout)
    return (
        f"A2A unread ({agent}):\n{compact}"
        if compact
        else f"A2A: no unread for {agent}"
    )


def top_cards(task: str, k: int = CARDS_K) -> list[str]:
    cards = kb_retrieve.query_index(task, kb_retrieve.load_index(), top_k=k)
    return [f"- {card.name}: {snippet(card.text)}" for card in cards]


def build_brief(
    *,
    task: str,
    state: dict[str, Any],
    claims_text: str,
    inbox_text: str,
    cards: list[str],
) -> str:
    mandates = [
        m.split(":", 1)[0].strip()
        for m in state.get("standing_mandates", [])
        if isinstance(m, str) and m
    ]
    headings = [
        h for h in state.get("active_headings", [])[:MAX_HEADINGS] if isinstance(h, str)
    ]
    lines = [f"TASK: {task.strip()}", "MANDATES: " + (", ".join(mandates) or "none")]
    if headings:
        lines.append("HEADINGS:")
        lines.extend(f"- {h}" for h in headings)
    lines.append(claims_text)
    if inbox_text:
        lines.append(inbox_text)
    if cards:
        lines.append("CARDS:")
        lines.extend(cards)
    text = "\n".join(lines)
    return (
        text
        if len(text) <= MAX_BRIEF_CHARS
        else text[: MAX_BRIEF_CHARS - 1].rstrip() + "…"
    )


def brief(
    task: str,
    paths: list[str] | None = None,
    agent: str | None = None,
    *,
    refresh_state: bool = False,
) -> str:
    """Compose the brief. ``agent`` defaults to ``A2A_AGENT_NAME``."""
    if not task.strip():
        raise ValueError("task must not be empty")
    agent = agent or os.environ.get("A2A_AGENT_NAME", "").strip() or None
    return build_brief(
        task=task,
        state=load_state(refresh=refresh_state),
        claims_text=claims_for_paths(paths or []),
        inbox_text=inbox_preview(agent),
        cards=top_cards(task),
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    b = sub.add_parser("brief", help="print the session brief")
    b.add_argument("--task", required=True)
    b.add_argument("--paths", nargs="*", default=[])
    b.add_argument("--agent", default=None)
    b.add_argument("--refresh-state", action="store_true")
    sub.add_parser("a2a-compact", help="compact an inbox listing from stdin")
    args = parser.parse_args(argv)
    if args.command == "a2a-compact":
        print(compact_inbox(sys.stdin.read()))
        return 0
    print(brief(args.task, args.paths, args.agent, refresh_state=args.refresh_state))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
