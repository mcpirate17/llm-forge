"""Leases for disposable worktrees, so an abandoned one has a name and a deadline.

Worktrees are scratch (`KB-GOV-01`), but they are also anonymous. `git worktree
list` reports that a directory exists; it never reports who created it, what for,
or when it should have been gone. On 2026-09-06/07 six trees accumulated on this
machine -- one of them holding 59 unstaged paths behind a tip identical to its
remote -- because nothing was ever going to ask for them back.

`conductor.workspace_hygiene.landed_worktrees` closed half of that: a worktree
whose work reached the live line is now demanded back. The half left open is the
worktree whose work *never* landed. It is not stale (the directory is there), not
landed (its commits are not upstream), and often not even dirty, so no check has
anything to say about it, and the session that made it is long gone.

A lease is the missing fact. It is one file inside the worktree naming the owner,
the purpose, and the hour it expires -- written by ``make worktree``, which is the
sanctioned way to create one. Two states then become reportable that were not
before: a tree past its deadline, and a tree with no lease at all, which means it
was created outside the sanctioned path.

Deliberately not enforcement. Nothing here deletes a worktree or blocks a commit:
a lease is a claim about intent, and a wrong one must never cost somebody their
uncommitted work. Expiry makes a tree *reportable*, which is all it takes -- the
trees accumulated because they were invisible, not because anyone defended them.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from collections.abc import Iterable
from datetime import UTC, datetime, timedelta
from pathlib import Path

LEASE_FILENAME = ".worktree-lease.json"  # Per-worktree state; gitignored. See .gitignore and KB-GOV-07.
DEFAULT_HOURS = 8.0
MAX_HOURS = 168.0
SCHEMA = "worktree-lease.v1"


class LeaseError(RuntimeError):
    """A lease could not be written, read, or trusted."""


def _now() -> datetime:
    return datetime.now(UTC)


def lease_path(worktree: Path) -> Path:
    return Path(worktree) / LEASE_FILENAME


def open_lease(
    worktree: Path,
    owner: str,
    purpose: str,
    hours: float = DEFAULT_HOURS,
    *,
    branch: str = "",
    now: datetime | None = None,
) -> dict[str, object]:
    """Record who holds ``worktree``, for what, and until when.

    ``purpose`` is required and is not decoration: the reason a stranded tree is
    hard to clean up is that nobody can tell whether the work in it still matters.
    """

    root = Path(worktree)
    if not root.is_dir():
        raise LeaseError(f"cannot lease {root}: not a directory")
    owner = owner.strip()
    purpose = purpose.strip()
    if not owner:
        raise LeaseError("a lease needs an owner: the session that will clean it up")
    if not purpose:
        raise LeaseError(
            "a lease needs a purpose; an unexplained worktree is exactly the thing "
            "nobody can decide to delete"
        )
    if not 0 < hours <= MAX_HOURS:
        raise LeaseError(f"lease hours must be in (0, {MAX_HOURS}]; got {hours!r}")

    opened = now or _now()
    record = {
        "schema": SCHEMA,
        "owner": owner,
        "purpose": purpose,
        "branch": branch,
        "worktree": str(root.resolve()),
        "opened_at": opened.isoformat(),
        "expires_at": (opened + timedelta(hours=hours)).isoformat(),
    }
    target = lease_path(root)
    scratch = target.with_suffix(".tmp")
    scratch.write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    scratch.replace(target)
    return record


def read_lease(worktree: Path) -> dict[str, object] | None:
    """The lease held by ``worktree``, or None when it holds none.

    Malformed is not the same as absent, and is raised rather than swallowed: a
    lease that cannot be parsed is a tree whose deadline nobody knows.
    """

    target = lease_path(worktree)
    if not target.is_file():
        return None
    try:
        record = json.loads(target.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise LeaseError(f"cannot read the lease at {target}: {exc}") from exc
    if not isinstance(record, dict) or record.get("schema") != SCHEMA:
        raise LeaseError(f"{target} is not a {SCHEMA} lease")
    for required in ("owner", "purpose", "expires_at"):
        if not record.get(required):
            raise LeaseError(f"the lease at {target} has no {required}")
    return record


def lease_state(
    worktrees: Iterable[Path], now: datetime | None = None
) -> list[dict[str, object]]:
    """Classify each worktree as leased, expired, or unleased.

    Takes the paths rather than discovering them, so the caller that already
    parsed ``git worktree list`` does not pay for a second parse and the two
    cannot disagree about which trees exist.
    """

    moment = now or _now()
    rows: list[dict[str, object]] = []
    for path in worktrees:
        root = Path(path)
        if not root.is_dir():
            continue
        record = read_lease(root)
        if record is None:
            rows.append({"worktree": str(root), "status": "unleased"})
            continue
        expires = datetime.fromisoformat(str(record["expires_at"]))
        overdue = moment - expires
        rows.append(
            {
                "worktree": str(root),
                "status": "expired" if overdue.total_seconds() > 0 else "leased",
                "owner": record["owner"],
                "purpose": record["purpose"],
                "branch": record.get("branch", ""),
                "expires_at": record["expires_at"],
                "overdue_minutes": max(0, int(overdue.total_seconds() // 60)),
            }
        )
    return rows


def is_linked_worktree(path: Path) -> bool:
    """True for a disposable worktree, False for the main checkout.

    Decided by git's own layout -- a linked worktree's ``.git`` is a *file*
    pointing at the common dir, the main checkout's is a directory -- and not by
    comparing against the root the caller happens to be running from. That
    comparison is what a caller inside a worktree gets wrong: it excludes itself
    and then reports the shared checkout as an unleased tree to delete.
    """

    dot_git = Path(path) / ".git"
    return dot_git.is_file()


def _linked_worktrees(repo: Path) -> list[Path]:
    """Every disposable worktree of ``repo``; never the main checkout."""

    done = subprocess.run(
        ["git", "worktree", "list", "--porcelain"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if done.returncode != 0:
        raise LeaseError(f"git worktree list failed: {done.stderr.strip()}")
    return [
        path
        for line in done.stdout.splitlines()
        if line.startswith("worktree ")
        for path in [Path(line[len("worktree ") :])]
        if is_linked_worktree(path)
    ]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="conductor.worktree_lease",
        description="Record and inspect the leases on disposable worktrees.",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    opener = sub.add_parser("open", help="write a lease into a worktree")
    opener.add_argument("worktree", type=Path)
    opener.add_argument("--owner", default=os.environ.get("GOVERNANCE_OWNER", ""))
    opener.add_argument("--purpose", required=True)
    opener.add_argument("--branch", default="")
    opener.add_argument("--hours", type=float, default=DEFAULT_HOURS)

    checker = sub.add_parser("check", help="classify every linked worktree")
    checker.add_argument("--repo", type=Path, default=Path.cwd())

    args = parser.parse_args(argv)
    if args.command == "open":
        owner = args.owner or Path(args.worktree).resolve().name
        record = open_lease(
            args.worktree,
            owner,
            args.purpose,
            args.hours,
            branch=args.branch,
        )
        print(json.dumps(record, indent=2))
        return 0

    rows = lease_state(_linked_worktrees(args.repo))
    print(json.dumps(rows, indent=2))
    return 1 if any(r["status"] != "leased" for r in rows) else 0


if __name__ == "__main__":
    sys.exit(main())
