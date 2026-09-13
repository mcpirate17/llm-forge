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
from collections.abc import Callable
from pathlib import Path
from typing import Final

from conductor.project_paths import host_root
ROOT: Final[Path] = host_root()
CURRENT_WORK_PATH: Final[Path] = ROOT / ".current_work.md"
MAX_TITLE_CHARS: Final[int] = 120
MAX_BODY_LINES: Final[int] = 12
MAX_BODY_CHARS: Final[int] = 1200
HEADING_RE: Final = re.compile(r"^## ", re.MULTILINE)
# The append succeeded but the active-state refresh behind it did not, so the log
# and the state the fleet reads from have diverged. Distinct from 2 (the append was
# rejected) because the repairs are opposite: here the entry IS on disk.
EXIT_STALE_STATE: Final[int] = 3


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
    on_refresh_error: Callable[[str], None] | None = None,
) -> str:
    """Write one heading to the log and refresh the active state behind it.

    The refresh is best-effort by design: the entry is already on disk when it runs,
    and unwinding a durable write because a derived cache could not be rebuilt would
    lose the status the caller came here to record. Best-effort is not the same as
    silent, though -- a swallowed refresh leaves `.current_work.md` describing work
    that the state file every other agent reads knows nothing about, and nothing
    said so. Every failure is now reported through `on_refresh_error` (stderr by
    default) and the CLI exits ``EXIT_STALE_STATE``.
    """
    entry = format_entry(owner, title, body, when=when)
    existing = (
        path.read_text(encoding="utf-8")
        if path.exists()
        else "# Active Coordination\n\n"
    )
    path.write_text(insert_newest(existing, entry), encoding="utf-8")
    report = on_refresh_error or _default_refresh_error
    refresher = refresh_state
    if refresher is None:
        try:
            from conductor.active_state import save_active_state

            refresher = save_active_state
        except Exception as exc:  # noqa: BLE001 - any import failure is the same fact
            refresher = None
            report(f"active state not refreshed: {type(exc).__name__}: {exc}")
    if callable(refresher):
        try:
            refresher()
        except Exception as exc:  # noqa: BLE001 - the entry is written either way
            report(f"active state refresh failed: {type(exc).__name__}: {exc}")
    return entry


def _default_refresh_error(message: str) -> None:
    print(f"{CURRENT_WORK_PATH.name} written, but {message}", file=sys.stderr)


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
    stale: list[str] = []

    def record(message: str) -> None:
        stale.append(message)
        _default_refresh_error(message)

    try:
        entry = append_status(
            args.owner, args.title, args.body, on_refresh_error=record
        )
    except HandoffError as exc:
        print(str(exc), file=sys.stderr)
        return 2
    # Printed before the exit code is decided: the entry IS written, and a reader
    # who sees only the failure would go looking for a status that is already there.
    print(entry.splitlines()[0])
    return EXIT_STALE_STATE if stale else 0


if __name__ == "__main__":
    raise SystemExit(main())
