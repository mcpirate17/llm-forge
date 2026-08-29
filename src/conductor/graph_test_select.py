#!/usr/bin/env python3
"""Graph-driven test selection utility for fast local feedback.

Queries the deterministic SQLite code-review graph (.code-review-graph/graph.db)
to identify test suites impacted by changed or specified files.

Scope: DIRECT dependencies only — a test file is selected when the graph holds an
edge from it to a changed file, or when it matches a ``test_<stem>.py`` naming
convention. Tests that reach changed code through an intermediate module are not
selected, and the graph itself can miss call edges. This is an advisory subset
for fast local feedback (``make test-graph``), never a governance gate.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import subprocess
import sys
from pathlib import Path, PurePosixPath
from typing import Final, Sequence

from conductor.candidate_review.git_source import repository_root

ROOT: Final[Path] = Path(__file__).resolve().parents[1]
SELECTION_SCOPE: Final = "direct-dependencies-and-conventions"


class GraphSelectError(RuntimeError):
    """The code-review graph is missing or unreadable; selection cannot be trusted."""


def graph_database_path(repo: Path) -> Path:
    return repo / ".code-review-graph" / "graph.db"


def git_changed_and_untracked_files(repo: Path) -> list[str]:
    """Retrieve modified, staged, and untracked files from Git."""
    paths: set[str] = set()
    diff_proc = subprocess.run(
        ["git", "diff", "--name-only", "HEAD"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if diff_proc.returncode == 0:
        for line in diff_proc.stdout.splitlines():
            line = line.strip()
            if line and (repo / line).is_file():
                paths.add(line)

    untracked_proc = subprocess.run(
        ["git", "ls-files", "--others", "--exclude-standard"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if untracked_proc.returncode == 0:
        for line in untracked_proc.stdout.splitlines():
            line = line.strip()
            if line and (repo / line).is_file():
                paths.add(line)

    return sorted(paths)


def query_graph_tests(repo: Path, source_paths: Sequence[str]) -> set[str]:
    """Query .code-review-graph/graph.db for test nodes that depend on source_paths."""
    db_path = graph_database_path(repo)
    if not db_path.is_file():
        raise GraphSelectError(
            f"code-review graph missing: {db_path}; run `code-review-graph update`"
        )

    absolute_paths = [str((repo / path).resolve()) for path in source_paths]
    if not absolute_paths:
        return set()

    tests: set[str] = set()
    uri = f"file:{db_path.as_posix()}?mode=ro&immutable=1"
    try:
        connection = sqlite3.connect(uri, uri=True, timeout=2.0)
        try:
            placeholders = ",".join("?" for _ in absolute_paths)
            query = f"""
                SELECT DISTINCT source.file_path
                FROM nodes AS target
                JOIN edges AS edge ON edge.target_qualified = target.qualified_name
                JOIN nodes AS source ON source.qualified_name = edge.source_qualified
                WHERE target.file_path IN ({placeholders}) AND source.is_test = 1
            """
            rows = connection.execute(query, absolute_paths).fetchall()
            for (file_path,) in rows:
                try:
                    rel = (
                        Path(file_path).resolve().relative_to(repo.resolve()).as_posix()
                    )
                    if (repo / rel).is_file():
                        tests.add(rel)
                except (ValueError, OSError):
                    continue
        finally:
            connection.close()
    except (sqlite3.Error, OSError) as exc:
        raise GraphSelectError(
            f"code-review graph unreadable: {db_path}: {exc}"
        ) from exc

    return tests


def is_test_file(path_str: str) -> bool:
    """Return True if the path is a test file by standard naming conventions."""
    name = PurePosixPath(path_str).name
    return name.startswith("test_") or name.endswith(
        ("_test.py", "_test.rs", ".test.js", ".test.ts", ".spec.ts", ".spec.js")
    )


def convention_tests_for_path(repo: Path, source_path: str) -> set[str]:
    """Identify convention-based test locations for a given source path."""
    tests: set[str] = set()
    path = PurePosixPath(source_path)
    stem = path.stem

    if is_test_file(source_path):
        if (repo / source_path).is_file():
            tests.add(source_path)
        return tests

    candidates = [
        path.parent / f"test_{stem}.py",
        path.parent / "tests" / f"test_{stem}.py",
        path.parent / f"{stem}_test.py",
        Path("tests") / f"test_{stem}.py",
    ]

    parts = path.parts
    if len(parts) > 1:
        candidates.append(Path(parts[0]) / "tests" / f"test_{stem}.py")

    for candidate in candidates:
        rel = candidate.as_posix()
        if (repo / rel).is_file():
            tests.add(rel)

    return tests


def select_tests_for_sources(repo: Path, source_paths: Sequence[str]) -> list[str]:
    """Combine graph and convention test selections for source paths."""
    selected: set[str] = set()
    sources = [
        s
        for s in source_paths
        if s.endswith((".py", ".rs", ".c", ".cpp", ".ts", ".js"))
    ]
    if not sources:
        return []

    # 1. Graph dependencies
    selected.update(query_graph_tests(repo, sources))

    # 2. Convention and self-test dependencies
    for source in sources:
        selected.update(convention_tests_for_path(repo, source))

    return sorted(selected)


def run_tests(
    repo: Path, test_paths: Sequence[str], pytest_args: Sequence[str] | None = None
) -> int:
    """Execute pytest on selected test files."""
    if not test_paths:
        print("No targeted tests selected.", file=sys.stdout)
        return 0

    cmd = [
        sys.executable,
        "-m",
        "pytest",
        "-o",
        "addopts=",
        *test_paths,
        *(pytest_args or ["-q", "--tb=short"]),
    ]
    print(
        f"Running {len(test_paths)} test file(s): {' '.join(test_paths)}",
        file=sys.stdout,
    )
    proc = subprocess.run(cmd, cwd=repo, check=False)
    return proc.returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "paths",
        nargs="*",
        help="Source files to select for; default: Git modified/untracked files.",
    )
    parser.add_argument(
        "--run", action="store_true", help="Execute pytest on selected tests"
    )
    parser.add_argument(
        "--json", action="store_true", help="Output selected tests as JSON array"
    )
    parser.add_argument(
        "--pytest-args",
        nargs="*",
        default=["-q", "--tb=short"],
        help="Arguments to pass to pytest when --run is enabled",
    )
    parser.add_argument("--repo", default=str(ROOT), help="Repository root path")

    args = parser.parse_args(argv)
    repo = repository_root(Path(args.repo))

    source_paths = args.paths if args.paths else git_changed_and_untracked_files(repo)
    try:
        selected_tests = select_tests_for_sources(repo, source_paths)
    except GraphSelectError as exc:
        print(f"graph-test-select ERROR: {exc}", file=sys.stderr)
        return 2

    if args.json:
        print(
            json.dumps(
                {
                    "scope": SELECTION_SCOPE,
                    "sources": source_paths,
                    "selected_tests": selected_tests,
                    "count": len(selected_tests),
                },
                indent=2,
            )
        )
        return 0

    if args.run:
        return run_tests(repo, selected_tests, args.pytest_args)

    for test in selected_tests:
        print(test)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
