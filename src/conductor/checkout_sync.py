"""Fast-forward the shared checkout onto the remote line, after snapshotting it.

The shared checkout is a mirror: work happens in worktrees and lands through a
PR, so the checkout has no reason ever to be anything but even with origin. On
2026-09-07 it was five commits behind with 314 dirty paths, and pulling it
looked risky precisely because nobody could tell what those paths were -- so it
stayed behind, and drifted further.

This is the actor half of ``workspace_hygiene.checkout_drift``, which only
reports. It is a separate module because hygiene promises to mutate nothing,
and a reporter that quietly starts merging is a reporter nobody can run.

Two rules make the merge safe enough to run unattended:

* Everything in the tree -- tracked modifications *and* untracked files -- is
  committed to a snapshot under ``refs/snapshots/`` before git is asked to move
  anything. The snapshot is a real commit off HEAD, so recovering is a checkout,
  not an archaeology exercise.
* The merge is ``--ff-only``. A fast-forward cannot rewrite a tracked file that
  differs locally, because git refuses the whole operation instead. Anything
  that is not a clean fast-forward is reported with the paths that blocked it
  and left alone; merging it is a judgement call, and this tool does not make
  judgement calls about somebody's uncommitted work.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
from datetime import UTC, datetime
from pathlib import Path

from conductor.workspace_hygiene import ROOT, checkout_drift

SNAPSHOT_NAMESPACE = "refs/snapshots/checkout-sync"


class SyncError(RuntimeError):
    """The checkout could not be synced, and was left exactly as it was."""


def _git(repo: Path, *args: str, env: dict[str, str] | None = None) -> str:
    done = subprocess.run(
        ["git", *args],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
        env=env,
    )
    if done.returncode != 0:
        raise SyncError(f"git {' '.join(args)} failed: {done.stderr.strip()}")
    return done.stdout


def snapshot(repo: Path, now: datetime | None = None) -> str | None:
    """Commit the whole working tree to a ref under ``refs/snapshots/``.

    Uses a private ``GIT_INDEX_FILE`` so the caller's staged state is untouched:
    this runs against a checkout somebody else may be mid-edit in, and staging
    their files would be a change they never asked for.

    Returns the ref, or None when the tree is clean and there is nothing to save.
    """

    if not _git(repo, "status", "--porcelain").strip():
        return None
    stamp = (now or datetime.now(UTC)).strftime("%Y%m%dT%H%M%SZ")
    ref = f"{SNAPSHOT_NAMESPACE}/{stamp}"
    with tempfile.TemporaryDirectory() as scratch:
        env = dict(os.environ, GIT_INDEX_FILE=str(Path(scratch) / "index"))
        _git(repo, "read-tree", "HEAD", env=env)
        _git(repo, "add", "-A", env=env)
        tree = _git(repo, "write-tree", env=env).strip()
    head = _git(repo, "rev-parse", "HEAD").strip()
    commit = _git(
        repo,
        "commit-tree",
        tree,
        "-p",
        head,
        "-m",
        f"snapshot(checkout): working tree before a fast-forward at {stamp}",
    ).strip()
    _git(repo, "update-ref", ref, commit)
    return ref


def sync(repo: Path = ROOT, *, dry_run: bool = False) -> dict[str, object]:
    """Snapshot, then fast-forward ``repo`` onto its remote integration line."""

    drift = checkout_drift(repo)
    result: dict[str, object] = dict(drift)
    result["snapshot"] = None
    if not drift["behind"]:
        result["outcome"] = "already even"
        return result
    if drift["ahead"]:
        raise SyncError(
            f"{repo} has {drift['ahead']} commit(s) the remote line does not; "
            "push them through a PR rather than fast-forwarding over them"
        )
    if drift["blocked_by"]:
        result["outcome"] = "blocked"
        return result
    if dry_run:
        result["outcome"] = "would fast-forward"
        return result

    result["snapshot"] = snapshot(repo)
    _git(repo, "merge", "--ff-only", str(drift["remote"]))
    result["outcome"] = "fast-forwarded"
    return result


def render(result: dict[str, object]) -> str:
    outcome = result["outcome"]
    remote = result["remote"]
    if outcome == "already even":
        return f"checkout is even with {remote}"
    if outcome == "blocked":
        blocked = result["blocked_by"]
        assert isinstance(blocked, list)
        lines = [
            f"checkout is {result['behind']} behind {remote} and cannot fast-forward:",
            f"  {len(blocked)} tracked file(s) differ locally and also change upstream",
            *(f"    {path}" for path in blocked[:20]),
            "commit or land those first; nothing was changed",
        ]
        return "\n".join(lines)
    if outcome == "would fast-forward":
        return f"checkout would fast-forward {result['behind']} commit(s) onto {remote}"
    saved = result["snapshot"]
    return "\n".join(
        [
            f"fast-forwarded {result['behind']} commit(s) onto {remote}",
            f"working tree saved at {saved}" if saved else "working tree was clean",
        ]
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="conductor.checkout_sync",
        description="Snapshot the working tree, then fast-forward onto the remote line.",
    )
    parser.add_argument("--repo", type=Path, default=ROOT)
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="report what a sync would do without touching the tree",
    )
    args = parser.parse_args(argv)
    result = sync(args.repo, dry_run=args.dry_run)
    print(render(result))
    return 3 if result["outcome"] == "blocked" else 0


if __name__ == "__main__":
    sys.exit(main())
