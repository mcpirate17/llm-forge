#!/usr/bin/env python3
"""Bounded status append for ``.current_work.md``.

Research dumps belong in ``research/notes/``. This helper is the only
supported agent write to the live log: one heading, max 12 body lines.
"""

from __future__ import annotations

import argparse
import datetime as dt
import re
import sys
from pathlib import Path
from typing import Final

ROOT: Final[Path] = Path(__file__).resolve().parents[1]
CURRENT_WORK_PATH: Final[Path] = ROOT / ".current_work.md"
MAX_TITLE_CHARS: Final[int] = 120
MAX_BODY_LINES: Final[int] = 12
MAX_BODY_CHARS: Final[int] = 1200
HEADING_RE: Final = re.compile(r"^## ", re.MULTILINE)


class HandoffError(ValueError):
    """Rejected status append."""


def validate_entry(owner: str, title: str, body: str) -> tuple[str, str, str]:
    owner_s = owner.strip()
    title_s = " ".join(title.strip().split())
    body_s = body.strip()
    if not owner_s:
        raise HandoffError("owner is required")
    if not title_s:
        raise HandoffError("title is required")
    if len(title_s) > MAX_TITLE_CHARS:
        raise HandoffError(f"title exceeds {MAX_TITLE_CHARS} chars")
    if not body_s:
        raise HandoffError("body is required")
    if len(body_s) > MAX_BODY_CHARS:
        raise HandoffError(f"body exceeds {MAX_BODY_CHARS} chars")
    line_count = len(body_s.splitlines())
    if line_count > MAX_BODY_LINES:
        raise HandoffError(f"body exceeds {MAX_BODY_LINES} lines ({line_count})")
    return owner_s, title_s, body_s


def format_entry(
    owner: str, title: str, body: str, when: dt.datetime | None = None
) -> str:
    owner_s, title_s, body_s = validate_entry(owner, title, body)
    stamp = (when or dt.datetime.now(dt.timezone.utc)).strftime("%Y-%m-%d ~%H:%M UTC")
    return f"## {title_s} — {stamp}, {owner_s}\n\n{body_s}\n\n"


def insert_newest(existing: str, entry: str) -> str:
    match = HEADING_RE.search(existing)
    if match:
        return existing[: match.start()] + entry + existing[match.start() :]
    text = existing if existing.endswith("\n") or not existing else existing + "\n"
    return text + entry


def append_status(
    owner: str,
    title: str,
    body: str,
    *,
    path: Path = CURRENT_WORK_PATH,
    when: dt.datetime | None = None,
    refresh_state: object | None = None,
) -> str:
    entry = format_entry(owner, title, body, when=when)
    existing = (
        path.read_text(encoding="utf-8")
        if path.exists()
        else "# Active Coordination\n\n"
    )
    path.write_text(insert_newest(existing, entry), encoding="utf-8")
    refresher = refresh_state
    if refresher is None:
        try:
            from conductor.active_state import save_active_state

            refresher = save_active_state
        except Exception:
            refresher = None
    if callable(refresher):
        try:
            refresher()
        except Exception:
            pass
    return entry


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Bounded .current_work.md status append"
    )
    sub = parser.add_subparsers(dest="command", required=True)
    append_p = sub.add_parser("append", help="prepend a short status heading")
    append_p.add_argument("--owner", required=True)
    append_p.add_argument("--title", required=True)
    append_p.add_argument("--body", required=True)
    args = parser.parse_args(argv)
    try:
        entry = append_status(args.owner, args.title, args.body)
    except HandoffError as exc:
        print(str(exc), file=sys.stderr)
        return 2
    print(entry.splitlines()[0])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
