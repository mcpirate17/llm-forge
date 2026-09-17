#!/usr/bin/env python3
"""One-call session bootstrap: mandates, live headings, claims on *your* paths,
A2A previews, and the top knowledge cards for the task.

Combines active state, bounded KB/memory recall, inbox previews, and path claims
in one ~2 KB payload when the local retrieval indexes are available. Missing
indexes degrade to an operational brief instead of breaking a clean checkout.
Available as a CLI (``python -m conductor.session_brief brief``) and as the
``session_brief_tool`` MCP tool. ``a2a-compact`` is the inbox preview filter.
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import subprocess
import sys
from datetime import datetime, timezone
from functools import lru_cache
from pathlib import Path
from typing import Any, Final

from conductor import kb_retrieve, memory_vectors
from conductor.a2a_session_start import (
    DEFAULT_MAX_CHARS,
    DEFAULT_MAX_MESSAGES,
    DEFAULT_PREVIEW_CHARS,
    SessionStartError,
    validate_compact_envelope,
)
from conductor.candidate_review.cli import compact_claims_text
from conductor.candidate_review.ownership import (
    OwnershipError,
    load_claims,
    paths_overlap,
)
from conductor.context_envelope import dedupe_fragments, fit_text
from conductor.project_paths import host_root, notes_db_path
from conductor.session_preamble import MAX_HEADINGS, load_state

ROOT: Final[Path] = host_root()
MAX_MSGS: Final[int] = DEFAULT_MAX_MESSAGES
PREVIEW_CHARS: Final[int] = DEFAULT_PREVIEW_CHARS
MAX_INBOX_CHARS: Final[int] = DEFAULT_MAX_CHARS
SNIPPET_CHARS: Final[int] = 240
CARDS_K: Final[int] = 3
MEMORY_K: Final[int] = 3
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


def is_compact_inbox(text: str) -> bool:
    """True when *text* is the compact inbox rendering, not a ``--full`` dump."""
    lines = text.splitlines()
    return bool(lines) and lines[0].startswith("A2A compact agent=")


def compact_inbox(
    text: str, *, max_msgs: int = MAX_MSGS, preview: int = PREVIEW_CHARS
) -> str:
    """Header + one-line preview per ``[UNREAD]`` message; never whole bodies.

    Input in the already-bounded compact form (``[<status>] <id> from=...``
    headers from ``agent_a2a inbox`` without ``--full``) passes through
    unchanged: it carries no bodies to strip, and the old filter silently
    erased it (0 bytes injected at session start, 2026-08-30 to 2026-09-01).
    """
    if is_compact_inbox(text):
        return text.strip()
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
                "--compact",
                "--max-messages",
                str(MAX_MSGS),
                "--preview-chars",
                str(PREVIEW_CHARS),
                "--max-chars",
                str(MAX_INBOX_CHARS),
                "--json",
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
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return "A2A: inbox unavailable (invalid compact response)"
    try:
        envelope = validate_compact_envelope(
            payload,
            identity=agent,
            max_messages=MAX_MSGS,
            preview_chars=PREVIEW_CHARS,
        )
    except SessionStartError:
        return "A2A: inbox unavailable (untrusted compact response)"
    compact = json.dumps(
        envelope, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    )
    if len(compact) > MAX_INBOX_CHARS:
        return "A2A: inbox unavailable (untrusted compact response)"
    if envelope["total"] == 0:
        return f"A2A: no unread for {agent}"
    return f"A2A unread ({agent}); full message by explicit show:\n{compact}"


@lru_cache(maxsize=8)
def _query_vector(task: str) -> list[float]:
    return kb_retrieve.embed_text(
        kb_retrieve.QUERY_INSTRUCT + task.strip(), purpose="query"
    )


def top_cards(task: str, k: int = CARDS_K) -> list[str]:
    try:
        index = kb_retrieve.load_index()

        def embedder(_text: str) -> list[float]:
            return _query_vector(task)

        try:
            cards = kb_retrieve.query_index(task, index, top_k=k, embedder=embedder)
        except TypeError as exc:
            if "unexpected keyword argument 'embedder'" not in str(exc):
                raise
            cards = kb_retrieve.query_index(task, index, top_k=k)
    except (OSError, RuntimeError, ValueError):
        return []
    return [f"- {card.name}: {snippet(card.text)}" for card in cards]


def memory_previews(task: str, k: int = MEMORY_K) -> list[str]:
    """Return compact semantic-memory hits using the KB query vector once."""

    try:
        rows, matrix = memory_vectors.load_sidecar()
        hits = memory_vectors.search(_query_vector(task), rows, matrix, top_k=k)
    except (OSError, RuntimeError, ValueError):
        return []
    return [
        f"- {hit['source']}/{hit['title']}: {snippet(hit['text'])} ({hit['path']})"
        for hit in hits
    ]


def task_previews(task: str, k: int = 3) -> list[str]:
    """Fetch cheap, local task/TODO previews without opening whole documents.

    The database is the host's prose index, resolved the same way ``index_notes``
    resolves it when it writes. This read ``index_notes.DB_PATH`` until
    2026-09-16, which was hardcoded to ``research/runs.db``: on a host that had
    split its databases the writer and this reader were pointed at different
    files, so every session's task previews came from whatever the run database
    happened to hold -- on the monorepo, an index frozen since the 09-14 split.
    """

    try:
        from conductor.index_notes import search_notes

        database = notes_db_path(ROOT)
        if not database.is_file():
            return []
        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
            rows = search_notes(connection, task, limit=k, source="tasks")
    except (OSError, RuntimeError, sqlite3.Error, ValueError):
        return []
    return [
        f"- {row['title']}: {snippet(row['snippet'])} ({row['path']})" for row in rows
    ]


def build_brief(
    *,
    task: str,
    state: dict[str, Any],
    claims_text: str,
    inbox_text: str,
    cards: list[str],
    task_hits: list[str] | None = None,
    memory_hits: list[str] | None = None,
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
    seen: set[str] = set()
    task_lines = dedupe_fragments(task_hits or [], seen=seen)
    memory_lines = dedupe_fragments(memory_hits or [], seen=seen)
    card_lines = dedupe_fragments(cards, seen=seen)
    if task_lines:
        lines.append("TASKS:")
        lines.extend(task_lines)
    if memory_lines:
        lines.append("MEMORY:")
        lines.extend(memory_lines)
    if card_lines:
        lines.append("CARDS:")
        lines.extend(card_lines)
    return fit_text("\n".join(lines), MAX_BRIEF_CHARS)


def brief(
    task: str,
    paths: list[str] | None = None,
    agent: str | None = None,
    *,
    refresh_state: bool | None = None,
) -> str:
    """Compose the brief. ``agent`` defaults to ``A2A_AGENT_NAME``."""
    if not task.strip():
        raise ValueError("task must not be empty")
    agent = agent or os.environ.get("A2A_AGENT_NAME", "").strip() or None
    memory_hits = memory_previews(task)
    return build_brief(
        task=task,
        state=load_state(refresh=refresh_state),
        claims_text=claims_for_paths(paths or []),
        inbox_text=inbox_preview(agent),
        cards=top_cards(task),
        task_hits=[] if memory_hits else task_previews(task),
        memory_hits=memory_hits,
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    b = sub.add_parser("brief", help="print the session brief")
    b.add_argument("--task", required=True)
    b.add_argument("--paths", nargs="*", default=[])
    b.add_argument("--agent", default=None)
    b.add_argument("--refresh-state", action="store_true", default=None)
    sub.add_parser("a2a-compact", help="compact an inbox listing from stdin")
    args = parser.parse_args(argv)
    if args.command == "a2a-compact":
        print(compact_inbox(sys.stdin.read()))
        return 0
    print(brief(args.task, args.paths, args.agent, refresh_state=args.refresh_state))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
