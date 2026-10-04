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
import os
import subprocess
import sys
from collections.abc import Sequence
from contextlib import nullcontext
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING, Any, Final, TypedDict

from conductor.candidate_review.contract_runtime import standalone_contract_runtime
from conductor.candidate_review.git_source import repository_root

if TYPE_CHECKING:
    from conductor.candidate_review.checks import ContractPlan

ROOT: Final[Path] = Path(__file__).resolve().parents[1]


class GraphTestPlan(TypedDict):
    paths: list[str]
    complete: bool
    scope: str
    reasons: list[str]
    generation: str | None
    metadata: dict[str, str]


class GraphSelectError(RuntimeError):
    """The code-review graph is missing or unreadable; selection cannot be trusted."""


def graph_database_path(repo: Path) -> Path:
    native = repo / ".forge" / "graph.db"
    return native if native.is_file() else repo / ".code-review-graph" / "graph.db"


def git_changed_and_untracked_files(repo: Path) -> list[str]:
    """Retrieve modified, staged, and untracked files from Git."""
    paths: set[str] = set()
    diff_proc = subprocess.run(
        ["git", "diff", "--no-renames", "--name-only", "HEAD"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if diff_proc.returncode == 0:
        for line in diff_proc.stdout.splitlines():
            line = line.strip()
            if line and (not (repo / line).exists() or (repo / line).is_file()):
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


def graph_test_plan(
    repo: Path,
    source_paths: Sequence[str],
    *,
    expected_head: str | None = None,
    inventory_root: Path | None = None,
) -> GraphTestPlan:
    """Return bounded transitive selection and explicit conservative fallback metadata."""
    from conductor._native import graph_context_native

    payload: dict[str, Any] = {"repo": str(repo), "paths": list(source_paths)}
    if expected_head is not None:
        payload["expected_head"] = expected_head
    if inventory_root is not None:
        payload["inventory_root"] = str(inventory_root)
    try:
        return json.loads(graph_context_native("test_selection", json.dumps(payload)))
    except ValueError as exc:
        raise GraphSelectError(str(exc)) from exc


def query_graph_tests(repo: Path, source_paths: Sequence[str]) -> set[str]:
    """Select through the shared native/external graph adapter."""
    if not source_paths:
        return set()
    return set(graph_test_plan(repo, source_paths)["paths"])


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
    sources = [
        s
        for s in source_paths
        if s.endswith((".py", ".rs", ".c", ".cpp", ".ts", ".js"))
    ]
    if not sources:
        return []

    return _selected_from_plan(repo, sources, graph_test_plan(repo, sources))


def _selected_from_plan(
    repo: Path, sources: Sequence[str], plan: GraphTestPlan
) -> list[str]:
    selected = {test for test in plan["paths"] if test.endswith(".py")}
    for source in sources:
        selected.update(
            test
            for test in convention_tests_for_path(repo, source)
            if test.endswith(".py")
        )
    return sorted(selected)


def run_tests(
    repo: Path,
    test_paths: Sequence[str],
    pytest_args: Sequence[str] | None = None,
    *,
    contract_plan: ContractPlan | None = None,
) -> int:
    """Execute selected pytest files and native Rust contracts."""
    contract_commands = contract_plan["commands"] if contract_plan else []
    if not test_paths and not contract_commands:
        print("No targeted tests selected.", file=sys.stdout)
        return 0

    runtime = (
        standalone_contract_runtime(repo, contract_plan)
        if contract_plan and contract_plan["targets"]
        else nullcontext({})
    )
    with runtime as contract_env:
        environment = (
            {
                **{
                    key: value
                    for key, value in os.environ.items()
                    if key not in {"PYO3_CONFIG_FILE", "PYTHONHOME"}
                },
                **contract_env,
            }
            if contract_env
            else None
        )
        exit_code = 0
        if test_paths:
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
            exit_code = subprocess.run(
                cmd, cwd=repo, env=environment, check=False
            ).returncode
        for command in contract_commands:
            print(
                f"Running Rust contracts: {', '.join(command['targets'])}",
                file=sys.stdout,
            )
            result = subprocess.run(
                command["argv"],
                cwd=command["cwd"],
                env=environment,
                timeout=900,
                check=False,
            )
            if result.returncode and not exit_code:
                exit_code = result.returncode
        return exit_code


def main(argv: list[str] | None = None) -> int:
    from conductor._native import contract_test_plan_native

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "paths",
        nargs="*",
        help="Source files to select for; default: Git modified/untracked files.",
    )
    parser.add_argument(
        "--run", action="store_true", help="Execute selected pytest and Rust contracts"
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Output the selected tests and contracts as JSON",
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
        graph_plan = (
            graph_test_plan(repo, source_paths)
            if source_paths
            else {
                "paths": [],
                "complete": True,
                "scope": "no-source-changes",
                "reasons": [],
                "generation": None,
                "metadata": {},
            }
        )
        selected_tests = _selected_from_plan(repo, source_paths, graph_plan)
        contract_plan: ContractPlan = json.loads(
            contract_test_plan_native(str(repo), list(source_paths))
        )
    except (GraphSelectError, ValueError) as exc:
        print(f"graph-test-select ERROR: {exc}", file=sys.stderr)
        return 2

    if args.json:
        print(
            json.dumps(
                {
                    "scope": graph_plan["scope"],
                    "complete": graph_plan["complete"],
                    "fallback_reasons": graph_plan["reasons"],
                    "generation": graph_plan["generation"],
                    "sources": source_paths,
                    "selected_tests": selected_tests,
                    "contract_targets": contract_plan["targets"],
                    "contract_test_paths": contract_plan["test_paths"],
                    "contract_commands": contract_plan["commands"],
                    "count": len(selected_tests) + len(contract_plan["targets"]),
                },
                indent=2,
            )
        )
        return 0

    if args.run:
        try:
            return run_tests(
                repo, selected_tests, args.pytest_args, contract_plan=contract_plan
            )
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
            print(f"graph-test-select ERROR: {exc}", file=sys.stderr)
            return 2

    for test in selected_tests:
        print(test)
    for target in contract_plan["targets"]:
        print(f"cargo:{target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
