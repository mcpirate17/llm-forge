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
* HEAD only ever fast-forwards, and only after every local edit that meets an
  incoming change is classified: ``identical`` to upstream (dropped, upstream
  wins trivially), ``carried`` (a 3-way ``git merge-file`` of HEAD / working
  tree / upstream merges clean, and the merge output is written back as an
  unstaged edit), or ``conflict`` / ``untracked-differs``. Any conflict refuses
  the whole sync with HEAD, index and tree untouched; resolving it is a judgement
  call about somebody's uncommitted work that this tool does not make. Staged
  edits to files upstream does not touch stay staged; a carried file loses its
  staged bit (the bytes survive).
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
_REFUSED = ("conflict", "untracked-differs")


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


def _git_bytes(repo: Path, *args: str) -> bytes:
    done = subprocess.run(["git", *args], cwd=repo, capture_output=True, check=False)
    if done.returncode != 0:
        raise SyncError(
            f"git {' '.join(args)} failed: {done.stderr.decode(errors='replace').strip()}"
        )
    return done.stdout


def _nul_list(repo: Path, *args: str) -> list[str]:
    raw = _git_bytes(repo, *args).decode()
    return [item for item in raw.split("\0") if item]


def _rev_blob(repo: Path, rev: str, path: str) -> str | None:
    done = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"{rev}:{path}"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    return done.stdout.strip() if done.returncode == 0 else None


def _three_way(repo: Path, path: str, remote: str) -> bytes | None:
    """Merged bytes of ``path`` (base HEAD, ours worktree, theirs remote), or None on conflict.

    Only regular files that exist on all three sides merge; a delete, a symlink or
    binary content is a conflict, never a guess.
    """

    work = repo / path
    base = _rev_blob(repo, "HEAD", path)
    theirs = _rev_blob(repo, remote, path)
    if base is None or theirs is None or work.is_symlink() or not work.is_file():
        return None
    with tempfile.TemporaryDirectory() as scratch:
        files = []
        for name, data in (
            ("ours", work.read_bytes()),
            ("base", _git_bytes(repo, "cat-file", "blob", base)),
            ("theirs", _git_bytes(repo, "cat-file", "blob", theirs)),
        ):
            target = Path(scratch) / name
            target.write_bytes(data)
            files.append(str(target))
        done = subprocess.run(
            [
                "git",
                "merge-file",
                "-p",
                "-L",
                "ours",
                "-L",
                "base",
                "-L",
                "theirs",
                *files,
            ],
            cwd=repo,
            capture_output=True,
            check=False,
        )
    return done.stdout if done.returncode == 0 else None


def _hash_worktree(repo: Path, path: str) -> str | None:
    work = repo / path
    if work.is_symlink() or not work.exists():
        return None
    return (
        _git_bytes(repo, "hash-object", f"--path={path}", "--", path).decode().strip()
    )


def classify(repo: Path, remote: str) -> tuple[dict[str, str], dict[str, bytes]]:
    """Classify every local edit that meets an incoming change.

    Returns ``(classification, merged)``. Classes: ``identical`` (working tree
    already equals upstream), ``carried`` (3-way merge is clean; bytes in
    ``merged``), ``conflict``, ``untracked-identical``, ``untracked-differs``.
    Nothing is written.
    """

    status = _nul_list(
        repo, "diff", "--name-status", "-z", "--no-renames", f"HEAD..{remote}"
    )
    incoming = dict(zip(status[1::2], status[0::2], strict=True))
    dirty = set(_nul_list(repo, "diff", "--name-only", "-z", "HEAD"))
    classification: dict[str, str] = {}
    merged: dict[str, bytes] = {}
    for path in sorted(set(incoming) & dirty):
        theirs = _rev_blob(repo, remote, path)
        local = _hash_worktree(repo, path)
        if local == theirs:
            classification[path] = "identical"
            continue
        result = _three_way(repo, path, remote)
        if result is None:
            classification[path] = "conflict"
        else:
            classification[path] = "carried"
            merged[path] = result
    added = {path for path, kind in incoming.items() if kind == "A"}
    for path in _nul_list(repo, "ls-files", "--others", "--exclude-standard", "-z"):
        if path in added:
            same = _hash_worktree(repo, path) == _rev_blob(repo, remote, path)
            classification[path] = (
                "untracked-identical" if same else "untracked-differs"
            )
    return classification, merged


def _conflicts(classification: dict[str, str]) -> list[str]:
    return sorted(p for p, kind in classification.items() if kind in _REFUSED)


def _restore(repo: Path, held: dict[str, bytes | None]) -> None:
    """Put back the local bytes of files ``_apply`` had reset, after a failed read-tree."""

    for path, data in held.items():
        target = repo / path
        if data is None:
            target.unlink(missing_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)


def _apply(
    repo: Path, remote: str, classification: dict[str, str], merged: dict[str, bytes]
) -> None:
    """Move ref, index and tree to ``remote`` and write carried edits back.

    Overlapping files are first returned to HEAD (their local bytes are held in
    memory and in the snapshot), then ``read-tree -m -u`` does the fast-forward
    of index and tree. Staged edits to files upstream does not touch stay staged;
    a carried file comes back as an unstaged edit on top of upstream.
    """

    old = _git(repo, "rev-parse", "HEAD").strip()
    new = _git(repo, "rev-parse", f"{remote}^{{commit}}").strip()
    tracked = [p for p, k in classification.items() if k in ("identical", "carried")]
    loose = [p for p, k in classification.items() if k == "untracked-identical"]
    held = {
        p: (repo / p).read_bytes() if (repo / p).is_file() else None
        for p in (*tracked, *loose)
    }
    try:
        for path in loose:
            (repo / path).unlink()
        if tracked:
            _git(repo, "checkout", "HEAD", "--", *tracked)
        _git(repo, "read-tree", "-m", "-u", old, new)
    except SyncError:
        _restore(repo, held)
        raise
    _git(
        repo,
        "update-ref",
        "-m",
        f"checkout-sync: fast-forward to {new[:12]}",
        "HEAD",
        new,
        old,
    )
    for path, data in merged.items():
        (repo / path).write_bytes(data)


def sync(repo: Path = ROOT, *, dry_run: bool = False) -> dict[str, object]:
    """Snapshot, then fast-forward ``repo`` onto its remote integration line.

    Local edits that meet an incoming change are classified first (see
    ``classify``). Any conflict refuses the whole sync with nothing changed.
    """

    drift = checkout_drift(repo)
    result: dict[str, object] = dict(drift)
    result["snapshot"] = None
    result["classification"] = {}
    if not drift["behind"]:
        result["outcome"] = "already even"
        return result
    if drift["ahead"]:
        raise SyncError(
            f"{repo} has {drift['ahead']} commit(s) the remote line does not; "
            "push them through a PR rather than fast-forwarding over them"
        )
    remote = str(drift["remote"])
    classification, merged = classify(repo, remote)
    result["classification"] = classification
    conflicts = _conflicts(classification)
    result["blocked_by"] = conflicts
    result["fast_forwardable"] = not conflicts
    if conflicts:
        result["outcome"] = "blocked"
        return result
    if dry_run:
        result["outcome"] = "would fast-forward"
        return result

    result["snapshot"] = snapshot(repo)
    _apply(repo, remote, classification, merged)
    result["outcome"] = "fast-forwarded"
    return result


def _classes(result: dict[str, object]) -> list[str]:
    classification = result["classification"]
    assert isinstance(classification, dict)
    return [f"    {kind:<19} {path}" for path, kind in sorted(classification.items())]


def render(result: dict[str, object]) -> str:
    outcome = result["outcome"]
    remote = result["remote"]
    if outcome == "already even":
        return f"checkout is even with {remote}"
    classes = _classes(result)
    if outcome == "blocked":
        blocked = result["blocked_by"]
        assert isinstance(blocked, list)
        lines = [
            f"checkout is {result['behind']} behind {remote} and cannot fast-forward:",
            f"  {len(blocked)} local edit(s) conflict with upstream (3-way merge failed or untracked file differs)",
            *classes[:40],
            "commit or land those first; nothing was changed",
        ]
        return "\n".join(lines)
    head = (
        f"checkout would fast-forward {result['behind']} commit(s) onto {remote}"
        if outcome == "would fast-forward"
        else f"fast-forwarded {result['behind']} commit(s) onto {remote}"
    )
    lines = [
        head,
        *(["  local edits meeting upstream:", *classes[:40]] if classes else []),
    ]
    if outcome == "fast-forwarded":
        saved = result["snapshot"]
        lines.append(
            f"working tree saved at {saved}" if saved else "working tree was clean"
        )
    return "\n".join(lines)


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
