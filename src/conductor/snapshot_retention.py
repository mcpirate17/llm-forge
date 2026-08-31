"""Index and expiry for ``refs/snapshots/**`` on origin.

``refs/snapshots/**`` is the repo's disaster-recovery namespace (KB-GOV-06's
"snapshot before acting", ``conductor.candidate_review.engine.snapshot_working_tree``,
``conductor.snapshot_exposure``). Nothing has ever pruned it --
``research/notes/branch_exposure_audit_2026-08-30.md`` found it at 80+ refs with no
retention, index, or expiry, "becoming the thing it exists to prevent."

Origin is the set of record: the repo's fetch refspec only tracks ``refs/heads/*``
(see ``.git/config``), so a local checkout's own ``refs/snapshots/**`` is session-
private litter that accumulates from every agent that ever ran a snapshot locally
without pushing it -- not the shared, durable set. This module reads origin's
snapshot refs directly with ``git ls-remote``; it never creates local scratch refs
or reads or touches ``refs/heads`` or the working tree.

Library API plus ``python -m conductor.snapshot_retention {list,expire}`` CLI.
Expiry defaults to a dry run: deleting a ref on origin is unrecoverable once the
remote runs ``git gc``, so actually deleting requires the separate ``--apply`` flag.
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SNAPSHOT_PREFIX = "refs/snapshots/"
SNAPSHOT_TIMESTAMP_FORMAT = "%Y%m%dT%H%M%SZ"
DEFAULT_TTL_DAYS = 21.0


class SnapshotRetentionError(RuntimeError):
    """Raised when the origin ref set cannot be read or acted on reliably."""


def _git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=False
    )


@dataclass(frozen=True)
class SnapshotRef:
    name: str  # full refs/snapshots/... name, as it exists on origin
    sha: str
    created_at: datetime | None

    @property
    def age_days(self) -> float | None:
        if self.created_at is None:
            return None
        return (datetime.now(UTC) - self.created_at).total_seconds() / 86400.0


def _snapshot_created_at(name: str) -> datetime | None:
    """Return the producer timestamp encoded in a supported snapshot ref.

    Git refs do not retain their push time, and the referenced commit can be much
    older than the recovery snapshot. Unknown legacy names therefore have no safe
    age and remain ineligible for automatic expiry.
    """
    if not name.startswith(SNAPSHOT_PREFIX):
        return None
    stamp = name.rsplit("/", 1)[-1]
    try:
        return datetime.strptime(stamp, SNAPSHOT_TIMESTAMP_FORMAT).replace(tzinfo=UTC)
    except ValueError:
        return None


def list_remote_snapshots(
    repo: Path = ROOT, remote: str = "origin"
) -> list[SnapshotRef]:
    """The durable set: every ``refs/snapshots/**`` ref that exists on ``remote``."""
    listed = _git(repo, "ls-remote", remote, f"{SNAPSHOT_PREFIX}*")
    if listed.returncode != 0:
        raise SnapshotRetentionError(
            f"listing {SNAPSHOT_PREFIX}* on {remote} failed: {listed.stderr.strip()}"
        )
    refs: list[SnapshotRef] = []
    for line in listed.stdout.splitlines():
        if not line.strip():
            continue
        sha, name = line.split(maxsplit=1)
        if name.startswith(SNAPSHOT_PREFIX):
            refs.append(
                SnapshotRef(
                    name=name,
                    sha=sha,
                    created_at=_snapshot_created_at(name),
                )
            )
    return refs


def expired(
    refs: list[SnapshotRef], ttl_days: float = DEFAULT_TTL_DAYS
) -> list[SnapshotRef]:
    if not math.isfinite(ttl_days) or ttl_days < 0:
        raise ValueError(f"ttl_days must be finite and non-negative, got {ttl_days!r}")
    return [ref for ref in refs if ref.age_days is not None and ref.age_days > ttl_days]


def delete_remote_snapshots(
    repo: Path, refs: list[SnapshotRef], *, remote: str = "origin"
) -> list[str]:
    """Push-delete each ref on ``remote``. Never touches ``refs/heads``.

    Irreversible once the remote garbage-collects: only pass refs the caller intends
    to lose. Returns the names that were actually deleted; a per-ref push failure is
    skipped rather than aborting the batch, so one stale lock doesn't block the rest.
    """
    invalid = [ref.name for ref in refs if not ref.name.startswith(SNAPSHOT_PREFIX)]
    if invalid:
        raise SnapshotRetentionError(
            "refusing to delete refs outside refs/snapshots/: " + ", ".join(invalid)
        )

    deleted: list[str] = []
    for ref in refs:
        done = _git(
            repo,
            "push",
            f"--force-with-lease={ref.name}:{ref.sha}",
            remote,
            f":{ref.name}",
        )
        if done.returncode == 0:
            deleted.append(ref.name)
    return deleted


def render_index(refs: list[SnapshotRef]) -> str:
    lines = [f"refs/snapshots/** on origin: {len(refs)} refs"]
    rows = [(ref, ref.age_days) for ref in refs]
    for ref, age in sorted(
        rows,
        key=lambda row: (row[1] is None, -(row[1] or 0.0)),
    ):
        age_label = "unknown" if age is None else f"{age:6.1f}d"
        lines.append(f"  {age_label:>7}  {ref.name}  {ref.sha[:12]}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Index and expire refs/snapshots/** on origin."
    )
    parser.add_argument("--repo", type=Path, default=ROOT)
    parser.add_argument("--remote", default="origin")
    sub = parser.add_subparsers(dest="command", required=True)

    list_cmd = sub.add_parser("list", help="index every snapshot ref on origin")
    list_cmd.add_argument("--json", action="store_true")

    expire_cmd = sub.add_parser(
        "expire", help="delete snapshot refs older than --ttl-days"
    )
    expire_cmd.add_argument("--ttl-days", type=float, default=DEFAULT_TTL_DAYS)
    expire_cmd.add_argument(
        "--apply", action="store_true", help="actually delete; default is a dry run"
    )
    expire_cmd.add_argument("--json", action="store_true")

    args = parser.parse_args(argv)
    refs = list_remote_snapshots(args.repo, args.remote)

    if args.command == "list":
        if args.json:
            print(
                json.dumps(
                    [
                        {
                            "name": r.name,
                            "sha": r.sha,
                            "age_days": (
                                round(r.age_days, 2) if r.age_days is not None else None
                            ),
                        }
                        for r in refs
                    ]
                )
            )
        else:
            print(render_index(refs))
        return 0

    stale = expired(refs, args.ttl_days)
    if not args.apply:
        if args.json:
            print(
                json.dumps(
                    {
                        "dry_run": True,
                        "ttl_days": args.ttl_days,
                        "would_delete": [r.name for r in stale],
                    }
                )
            )
        else:
            print(
                f"DRY RUN -- {len(stale)}/{len(refs)} refs older than "
                f"{args.ttl_days:g}d would be deleted (pass --apply to delete):"
            )
            print(render_index(stale))
        return 0

    deleted = delete_remote_snapshots(args.repo, stale, remote=args.remote)
    if args.json:
        print(json.dumps({"ttl_days": args.ttl_days, "deleted": deleted}))
    else:
        print(f"deleted {len(deleted)}/{len(stale)} refs older than {args.ttl_days:g}d")
    return 0 if len(deleted) == len(stale) else 1


if __name__ == "__main__":
    raise SystemExit(main())
