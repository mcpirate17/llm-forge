"""Retention for A2A message stores: tombstone read messages after a grace period.

Agreed split with codex-phase22 (2026-08-30, A2A): codex-phase22 owns the
agents.json registry/liveness redesign in `conductor/agent_a2a.py`; this module
owns message-store retention as a separate helper so neither seat needs a live
claim on the other's file. Integration into `agent_a2a.py` (wiring this into
`serve`/a scheduled sweep) is codex-phase22's handoff, not done here.

Policy (Tim-authorized, 2026-08-30): a message is eligible for tombstoning only
if it has been READ and the read happened at least `grace` ago. Unread messages
are never touched, regardless of age -- there is no task-completion signal here,
only read-status + time. Tombstoning clears `body` and `data_json` but keeps
`message_id`, `sender`, `recipient`, `direction`, `created_at`, and `read_at`, so
the fact that a conversation happened stays reconstructable even after its
content is gone.
"""

from __future__ import annotations

import argparse
import sqlite3
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path

DEFAULT_GRACE = timedelta(hours=48)
TOMBSTONE_BODY = "[tombstoned: read, past retention grace period]"


@dataclass(frozen=True)
class RetentionResult:
    store: Path
    tombstoned: int
    scanned: int


def _parse_iso(value: str) -> datetime:
    # created_at/read_at are ISO 8601 with a trailing "+00:00"-style offset,
    # written by agent_a2a.py's own datetime.isoformat() calls.
    return datetime.fromisoformat(value)


def tombstone_expired_messages(
    store: Path,
    *,
    now: datetime | None = None,
    grace: timedelta = DEFAULT_GRACE,
    dry_run: bool = False,
) -> RetentionResult:
    """Tombstone read messages in one store.sqlite whose read_at is older than grace.

    Never touches a row with read_at IS NULL. Idempotent: a row already carrying
    the tombstone body is skipped, so re-running costs a scan, not a rewrite.
    """
    if not store.is_file():
        raise FileNotFoundError(f"no such A2A store: {store}")
    now = now or datetime.now(timezone.utc)
    cutoff = now - grace

    conn = sqlite3.connect(store, timeout=5.0)
    try:
        conn.row_factory = sqlite3.Row
        rows = conn.execute(
            """
            SELECT direction, message_id, read_at, body
            FROM messages
            WHERE read_at IS NOT NULL AND body != ?
            """,
            (TOMBSTONE_BODY,),
        ).fetchall()

        scanned = len(rows)
        to_tombstone: list[tuple[str, str]] = []
        for row in rows:
            read_at = _parse_iso(row["read_at"])
            if read_at <= cutoff:
                to_tombstone.append((row["direction"], row["message_id"]))

        if to_tombstone and not dry_run:
            conn.executemany(
                """
                UPDATE messages
                SET body = ?, data_json = NULL
                WHERE direction = ? AND message_id = ?
                """,
                [
                    (TOMBSTONE_BODY, direction, message_id)
                    for direction, message_id in to_tombstone
                ],
            )
            conn.commit()
    finally:
        conn.close()

    return RetentionResult(store=store, tombstoned=len(to_tombstone), scanned=scanned)


def sweep(
    state_dir: Path,
    *,
    now: datetime | None = None,
    grace: timedelta = DEFAULT_GRACE,
    dry_run: bool = False,
    only: str | None = None,
) -> list[RetentionResult]:
    """Run tombstone_expired_messages over every store.sqlite under state_dir."""
    results: list[RetentionResult] = []
    for store in sorted(state_dir.glob("*/store.sqlite")):
        if only is not None and store.parent.name != only:
            continue
        results.append(
            tombstone_expired_messages(store, now=now, grace=grace, dry_run=dry_run)
        )
    return results


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    run = sub.add_parser("run", help="tombstone eligible messages across A2A stores")
    run.add_argument("--state-dir", type=Path, default=Path(".agents/a2a"))
    run.add_argument(
        "--grace-hours", type=float, default=DEFAULT_GRACE.total_seconds() / 3600
    )
    run.add_argument(
        "--store", default=None, help="only this agent's store (by directory name)"
    )
    run.add_argument("--dry-run", action="store_true")

    args = parser.parse_args(argv)

    if args.command == "run":
        results = sweep(
            args.state_dir,
            grace=timedelta(hours=args.grace_hours),
            dry_run=args.dry_run,
            only=args.store,
        )
        total = sum(r.tombstoned for r in results)
        for r in results:
            if r.tombstoned or r.scanned:
                print(
                    f"{r.store.parent.name}: tombstoned {r.tombstoned}/{r.scanned} read messages"
                )
        verb = "would tombstone" if args.dry_run else "tombstoned"
        print(f"total: {verb} {total} messages across {len(results)} stores")
        return 0

    parser.error(f"unknown command: {args.command}")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
