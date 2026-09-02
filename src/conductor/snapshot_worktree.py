#!/usr/bin/env python3
"""Disposable git snapshots for autonomous mutation from a dirty host worktree."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator

SOURCE_SUFFIXES = {
    ".c",
    ".cc",
    ".cpp",
    ".cu",
    ".h",
    ".hpp",
    ".js",
    ".json",
    ".jsx",
    ".md",
    ".py",
    ".pyx",
    ".pxd",
    ".rs",
    ".sh",
    ".toml",
    ".ts",
    ".tsx",
    ".yaml",
    ".yml",
}
EXCLUDED_PREFIXES = (
    "research/reports/",
    "research/notes/",
    "tasks/",
)


@dataclass(frozen=True, slots=True)
class Snapshot:
    commit: str
    worktree: Path
    included_untracked: tuple[str, ...]


def _run(
    command: list[str], repo: Path, *, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(
        command,
        cwd=repo,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout or "git command failed").strip()
        raise RuntimeError(f"{' '.join(command)} failed: {detail[:2000]}")
    return proc


def snapshot_untracked_paths(repo: Path) -> list[str]:
    """Return source/config files needed by a snapshot, excluding generated artifacts."""
    output = _run(["git", "ls-files", "--others", "--exclude-standard"], repo).stdout
    return sorted(
        path
        for path in output.splitlines()
        if path
        and not path.startswith(EXCLUDED_PREFIXES)
        and Path(path).suffix.lower() in SOURCE_SUFFIXES
    )


def _snapshot_commit(repo: Path, root: Path) -> tuple[str, tuple[str, ...]]:
    index = root / "index"
    env = os.environ.copy()
    env.update(
        {
            "GIT_INDEX_FILE": str(index),
            "GIT_AUTHOR_NAME": "Audit Orchestrator Snapshot",
            "GIT_AUTHOR_EMAIL": "orchestrator@localhost",
            "GIT_COMMITTER_NAME": "Audit Orchestrator Snapshot",
            "GIT_COMMITTER_EMAIL": "orchestrator@localhost",
        }
    )
    _run(["git", "read-tree", "HEAD"], repo, env=env)
    _run(["git", "add", "-u"], repo, env=env)
    untracked = tuple(snapshot_untracked_paths(repo))
    for start in range(0, len(untracked), 100):
        _run(["git", "add", "--", *untracked[start : start + 100]], repo, env=env)
    tree = _run(["git", "write-tree"], repo, env=env).stdout.strip()
    parent = _run(["git", "rev-parse", "HEAD"], repo).stdout.strip()
    commit = _run(
        ["git", "commit-tree", tree, "-p", parent, "-m", "orchestrator snapshot"],
        repo,
        env=env,
    ).stdout.strip()
    return commit, untracked


@contextmanager
def isolated_snapshot(repo: Path) -> Iterator[Snapshot]:
    """Create a clean detached worktree without changing the host index or worktree."""
    root = Path(tempfile.mkdtemp(prefix="llm-orchestrator-snapshot-"))
    worktree = root / "worktree"
    added = False
    try:
        commit, untracked = _snapshot_commit(repo, root)
        _run(["git", "worktree", "add", "--detach", str(worktree), commit], repo)
        added = True
        yield Snapshot(commit, worktree, untracked)
    finally:
        if added:
            subprocess.run(
                ["git", "worktree", "remove", "--force", str(worktree)],
                cwd=repo,
                capture_output=True,
                text=True,
                check=False,
            )
            subprocess.run(
                ["git", "worktree", "prune"],
                cwd=repo,
                capture_output=True,
                text=True,
                check=False,
            )
        shutil.rmtree(root, ignore_errors=True)
