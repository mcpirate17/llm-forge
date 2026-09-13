"""Disposable git snapshots for autonomous mutation from a dirty host worktree."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

from conductor.project_paths import (
    DEFAULT_MUTATION_REGISTRY,
    ProjectPathError,
    conductor_table,
    notes_relative,
)

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
    f"{DEFAULT_MUTATION_REGISTRY.parent}/receipts/",
    "research/reports/",
    "tasks/",
)
# Path parts that mark test material: everything under (or named as) one of
# these is test fixture data, where the file extension says nothing about
# whether the file is needed. `native/forge/tests/fixtures/bash_impact_tree`
# keeps `.db` payloads, `.txt` trees and extensionless symlinks there.
FIXTURE_PARTS = frozenset({"tests", "fixtures"})
FIXTURE_PART_PREFIX = "test_"


def _excluded_prefixes(repo: Path) -> tuple[str, ...]:
    """Exclusions for one repo: the fixed set plus its configured notes tree."""
    return (*EXCLUDED_PREFIXES, f"{notes_relative(repo)}/")


def _fixture_path(path: str) -> bool:
    """A path inside test material, where the suffix allowlist cannot apply."""
    return any(
        part in FIXTURE_PARTS or part.startswith(FIXTURE_PART_PREFIX)
        for part in PurePosixPath(path).parts
    )


def _configured_extra_suffixes(repo: Path) -> frozenset[str]:
    """Snapshot suffixes the host added via ``[tool.conductor]``.

    Fixture trees are admitted by path, not suffix, so this answers for the
    host whose data files live outside any ``tests/``/``fixtures/`` tree yet
    must still reach the snapshot -- the configuration point for the case the
    path rule deliberately does not guess at.
    """
    raw = conductor_table(repo).get("snapshot_extra_suffixes")
    if raw is None:
        return frozenset()
    source = "[tool.conductor].snapshot_extra_suffixes"
    if not isinstance(raw, (list, tuple)) or not all(
        isinstance(item, str) for item in raw
    ):
        raise ProjectPathError(f"{source} must be a list of file suffixes")
    out: set[str] = set()
    for item in raw:
        text = item.strip().lower()
        if not text:
            raise ProjectPathError(f"{source} must not contain an empty suffix")
        out.add(text if text.startswith(".") else f".{text}")
    return frozenset(out)


def snapshot_python(repo: Path) -> str:
    """The interpreter a snapshot exports for tests that drive Python.

    A snapshot is a git tree and ``.venv`` is gitignored, so a test that shells
    out to Python (the forge parity twins are the standing case) finds no
    interpreter that can import ``conductor`` inside the worktree and the
    campaign dies at its own baseline. The venv is never copied -- the host's
    interpreter is exported instead, as ``CONDUCTOR_SNAPSHOT_PYTHON``:
    ``[tool.conductor].snapshot_python`` when the host needs a specific one,
    else ``sys.executable`` of whatever built the snapshot, which is by
    construction an interpreter with ``conductor`` importable.
    """

    raw = conductor_table(repo).get("snapshot_python")
    if raw is None:
        return sys.executable
    source = "[tool.conductor].snapshot_python"
    if not isinstance(raw, str) or not raw.strip():
        raise ProjectPathError(f"{source} must be a path to a Python interpreter")
    return raw


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
    """Return source/config files needed by a snapshot, excluding generated artifacts.

    Every file under a ``tests/``, ``fixtures/`` or ``test_*`` path rides
    along regardless of suffix: a fixture tree keeps `.db` payloads, plain
    text and extensionless symlinks, and dropping them sent otherwise green
    campaigns home as BASELINE_FAILED. Everywhere else the suffix allowlist
    (plus the host's ``snapshot_extra_suffixes``) still decides.
    """
    output = _run(["git", "ls-files", "--others", "--exclude-standard"], repo).stdout
    excluded = _excluded_prefixes(repo)
    allowed = SOURCE_SUFFIXES | _configured_extra_suffixes(repo)
    return sorted(
        path
        for path in output.splitlines()
        if path
        and not path.startswith(excluded)
        and (_fixture_path(path) or Path(path).suffix.lower() in allowed)
    )


def _snapshot_commit(
    repo: Path, snapshot_repo: Path, root: Path
) -> tuple[str, tuple[str, ...]]:
    index = root / "index"
    env = os.environ.copy()
    env.update(
        {
            "GIT_DIR": str(snapshot_repo / ".git"),
            "GIT_WORK_TREE": str(repo),
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
    """Create a clean snapshot without writing objects into the host repository."""
    root = Path(tempfile.mkdtemp(prefix="llm-orchestrator-snapshot-"))
    worktree = root / "worktree"
    try:
        _run(
            [
                "git",
                "clone",
                "--shared",
                "--no-checkout",
                "--quiet",
                str(repo.resolve()),
                str(worktree),
            ],
            repo,
        )
        commit, untracked = _snapshot_commit(repo, worktree, root)
        _run(["git", "checkout", "--detach", "--quiet", commit], worktree)
        yield Snapshot(commit, worktree, untracked)
    finally:
        shutil.rmtree(root, ignore_errors=True)
