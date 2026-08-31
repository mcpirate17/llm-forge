"""Snapshot-on-detect: the missing consumer of ``workspace_hygiene``'s EXPOSED signal.

``research.tools.workspace_hygiene`` and ``conductor.branch_policy`` are deliberately
report-only (see their module docstrings) -- detecting a branch with no push in
``STALE_PUSH_HOURS`` or no PR in ``STALE_PR_HOURS`` never mutates git state. Nothing
acted on that signal until now: ``research/notes/branch_exposure_audit_2026-08-30.md``
found five exposed branches whose only protection was an agent noticing and
hand-pushing a snapshot ref *during an audit*, after the fact. This module is the
consumer -- it re-runs the same detection and pushes a recovery ref for every branch
it finds, so the ref exists before a force-push or a branch deletion can make that
tip unrecoverable.

Idempotent: a branch already snapshotted at its current tip is skipped, so running
this on every session start (or a cron) does not multiply refs -- see
``conductor.snapshot_retention`` for pruning the refs this does create.

Library API plus ``python -m conductor.snapshot_exposure`` CLI.
"""

from __future__ import annotations

import argparse
import json
import subprocess
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path

from research.tools import workspace_hygiene

ROOT = Path(__file__).resolve().parents[1]
SNAPSHOT_PREFIX = "refs/snapshots/branches"


class SnapshotExposureError(RuntimeError):
    """Raised when git state cannot be trusted enough to act on."""


def _git(repo: Path, *args: str) -> str:
    done = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=False
    )
    if done.returncode != 0:
        raise SnapshotExposureError(
            f"git {' '.join(args)} failed: {done.stderr.strip()}"
        )
    return done.stdout


@dataclass(frozen=True)
class SnapshotAction:
    branch: str
    sha: str
    ref: str
    created: bool  # a new local ref was created this call
    pushed: bool  # ref is confirmed present on `remote` after this call


def _existing_snapshot_refs(repo: Path, branch: str) -> dict[str, str]:
    """sha -> the first existing ``refs/snapshots/branches/<branch>/**`` ref at it."""
    out = subprocess.run(
        [
            "git",
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            f"{SNAPSHOT_PREFIX}/{branch}",
        ],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    ).stdout
    mapping: dict[str, str] = {}
    for line in out.splitlines():
        if not line.strip():
            continue
        sha, ref = line.split(" ", 1)
        mapping.setdefault(sha, ref)
    return mapping


def _push_ref(repo: Path, ref: str, remote: str) -> bool:
    return (
        subprocess.run(
            ["git", "push", remote, ref],
            cwd=repo,
            capture_output=True,
            text=True,
            check=False,
        ).returncode
        == 0
    )


def _ref_on_remote(repo: Path, ref: str, remote: str) -> bool:
    return (
        subprocess.run(
            ["git", "ls-remote", "--exit-code", remote, ref],
            cwd=repo,
            capture_output=True,
            text=True,
            check=False,
        ).returncode
        == 0
    )


def snapshot_stale_branches(
    repo: Path = ROOT,
    *,
    push: bool = True,
    remote: str = "origin",
    stamp: str | None = None,
) -> list[SnapshotAction]:
    """Create (and, by default, push) a recovery ref for every EXPOSED branch tip.

    Reads ``workspace_hygiene.stale_feature_branches`` -- the same detection the
    session banner and ``branch_policy exposed`` already surface -- and never touches
    ``refs/heads/**`` itself, only adds to ``refs/snapshots/branches/**``.

    Idempotent on two axes: a tip already covered by a local ref is reused rather than
    duplicated, and that reused ref is still pushed if it was never confirmed on
    ``remote`` -- a prior ``--no-push`` (or a failed push) run must not make a later
    real run think the branch is already durable.
    """
    live_ref = _git(repo, "rev-parse", "--abbrev-ref", "HEAD").strip()
    rows, _gh_available = workspace_hygiene.stale_feature_branches(live_ref, repo)
    use_stamp = stamp or datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    actions: list[SnapshotAction] = []
    for row in rows:
        branch = str(row["branch"])
        sha = _git(repo, "rev-parse", branch).strip()
        existing = _existing_snapshot_refs(repo, branch)
        created = sha not in existing
        if created:
            ref = f"{SNAPSHOT_PREFIX}/{branch}/{use_stamp}"
            _git(repo, "update-ref", ref, sha)
        else:
            ref = existing[sha]
        pushed = False
        if push:
            pushed = (not created and _ref_on_remote(repo, ref, remote)) or _push_ref(
                repo, ref, remote
            )
        actions.append(
            SnapshotAction(
                branch=branch, sha=sha, ref=ref, created=created, pushed=pushed
            )
        )
    return actions


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Push a recovery ref for every EXPOSED branch (no push/PR consumer)."
    )
    parser.add_argument("--repo", type=Path, default=ROOT)
    parser.add_argument("--remote", default="origin")
    parser.add_argument(
        "--no-push",
        action="store_true",
        help="create the local ref only, do not push it",
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    actions = snapshot_stale_branches(
        args.repo, push=not args.no_push, remote=args.remote
    )

    if args.json:
        print(json.dumps([asdict(a) for a in actions]))
    else:
        if not actions:
            print("no EXPOSED branches")
        for action in actions:
            if args.no_push:
                verb = "SNAPSHOTTED (local only)" if action.created else "OK (local)"
                print(f"{verb}: {action.branch} -> {action.ref}")
            elif action.pushed:
                verb = "SNAPSHOTTED" if action.created else "OK (already durable)"
                print(f"{verb}: {action.branch} -> {action.ref} (on {args.remote})")
            else:
                print(
                    f"WARNING: local snapshot {action.ref} not confirmed on "
                    f"{args.remote} -- not yet durable"
                )

    failed_push = [a for a in actions if not args.no_push and not a.pushed]
    return 1 if failed_push else 0


if __name__ == "__main__":
    raise SystemExit(main())
