"""Inventory, changed-test evidence, and campaign scaffolding for mutation testing.

The runner in ``conductor.mutation_testing`` stays the receipt authority. This
module discovers test files, checks them against registered PASS receipts, and
scaffolds a campaign stub. It does not generate mutants or run campaigns.
"""

from __future__ import annotations

import argparse
import ast
import json
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path, PurePosixPath
from typing import Any

from conductor._native import (
    is_mutation_test_path_native,
    mutation_git_paths_native,
    mutation_registry_patterns_native,
    mutation_rust_test_surface_native,
    mutation_test_inventory_native,
    normalize_mutation_path_native,
    plan_mutation_scaffold_native,
    should_skip_mutation_path_native,
)

from conductor.mutation_testing import (
    CANONICAL_TEST_PATTERNS,
    REPO_ROOT,
    CampaignError,
    verify_evidence,
)

SKIP_DIRECTORY_NAMES = frozenset(
    {
        ".git",
        ".hg",
        ".svn",
        ".venv",
        "venv",
        "node_modules",
        "__pycache__",
        ".codex",
        ".grok",
        ".tox",
        "dist",
        "build",
        "worktrees",
        ".mypy_cache",
        ".ruff_cache",
        ".pytest_cache",
        ".code-review-graph",
    }
)
COVERAGE_SCHEMA = "llm.mutation-testing.coverage.v1"
DEFAULT_REGISTRY = Path("conductor/mutation_campaigns/registry.json")


def _native_or_campaign[T](operation: Callable[..., T], *args: object) -> T:
    try:
        return operation(*args)
    except ValueError as exc:
        raise CampaignError(str(exc)) from exc


def _safe_relative_path(value: str, label: str) -> str:
    return _native_or_campaign(normalize_mutation_path_native, value, label)


def _registry_patterns(registry_path: Path, repo_root: Path) -> tuple[str, ...]:
    return tuple(
        _native_or_campaign(
            mutation_registry_patterns_native,
            str(repo_root),
            str(registry_path),
            list(CANONICAL_TEST_PATTERNS),
        )
    )


def is_test_path(path: str, patterns: Sequence[str]) -> bool:
    """Return whether a repository-relative path matches mutation test patterns."""

    # The native hot path implements the canonical repository glob surface.
    # Preserve ``PurePosixPath.match`` semantics for the less common character-
    # class syntax instead of silently narrowing this public helper's contract.
    if any("[" in pattern for pattern in patterns):
        candidate = PurePosixPath(path.replace("\\", "/"))
        return any(candidate.match(pattern.replace("\\", "/")) for pattern in patterns)
    return _native_or_campaign(
        is_mutation_test_path_native,
        path,
        list(patterns),
    )


def is_rust_test_surface(path: str, *, repo_root: Path = REPO_ROOT) -> bool:
    """Return whether a Rust source declares tests, reading the file to decide.

    Rust puts unit tests in the module they test, so the glob patterns that
    answer :func:`is_test_path` for every other language answer nothing here.
    This is the second half of the inventory's test-surface question, and the
    only half a filename cannot settle.
    """

    return mutation_rust_test_surface_native(str(repo_root), path)


def _git_paths(repo_root: Path, args: Sequence[str]) -> tuple[str, ...]:
    return tuple(
        _native_or_campaign(
            mutation_git_paths_native,
            str(repo_root),
            list(args),
        )
    )


def _should_skip(relative: PurePosixPath) -> bool:
    return should_skip_mutation_path_native(
        str(relative),
        sorted(SKIP_DIRECTORY_NAMES),
    )


def discover_test_paths(
    registry_path: Path | None = None,
    *,
    repo_root: Path = REPO_ROOT,
    include_untracked: bool = True,
) -> tuple[str, ...]:
    """Return git-visible test files matching the mutation registry patterns."""

    registry = registry_path or (repo_root / DEFAULT_REGISTRY)
    return tuple(
        _native_or_campaign(
            mutation_test_inventory_native,
            str(repo_root),
            str(registry),
            list(CANONICAL_TEST_PATTERNS),
            sorted(SKIP_DIRECTORY_NAMES),
            "all",
            include_untracked,
        )
    )


def git_changed_test_paths(
    registry_path: Path | None = None,
    *,
    repo_root: Path = REPO_ROOT,
) -> tuple[str, ...]:
    """Return mutation-eligible tests that differ from HEAD, including untracked."""

    registry = registry_path or (repo_root / DEFAULT_REGISTRY)
    return tuple(
        _native_or_campaign(
            mutation_test_inventory_native,
            str(repo_root),
            str(registry),
            list(CANONICAL_TEST_PATTERNS),
            sorted(SKIP_DIRECTORY_NAMES),
            "changed",
            True,
        )
    )


def coverage_report(
    registry_path: Path | None = None,
    *,
    repo_root: Path = REPO_ROOT,
    include_untracked: bool = True,
) -> dict[str, Any]:
    """Check every discovered test file against current PASS receipts."""

    registry = registry_path or (repo_root / DEFAULT_REGISTRY)
    tests = discover_test_paths(
        registry, repo_root=repo_root, include_untracked=include_untracked
    )
    result = verify_evidence(registry, tests, repo_root=repo_root)
    covered = [row["path"] for row in result.get("evidence", [])]
    missing = [row["path"] for row in result.get("missing_evidence", [])]
    return {
        "schema_version": COVERAGE_SCHEMA,
        "status": result["status"],
        "enforcement": "repository_inventory",
        "registry": registry.resolve().relative_to(repo_root.resolve()).as_posix(),
        "total_test_files": len(tests),
        "covered_test_files": len(covered),
        "missing_test_files": len(missing),
        "evidence": result.get("evidence", []),
        "missing_evidence": result.get("missing_evidence", []),
        "malformed_receipts": result.get("malformed_receipts", []),
    }


def verify_changed(
    registry_path: Path | None = None,
    *,
    repo_root: Path = REPO_ROOT,
) -> dict[str, Any]:
    """Require PASS receipts for git-changed and untracked test files."""

    registry = registry_path or (repo_root / DEFAULT_REGISTRY)
    tests = git_changed_test_paths(registry, repo_root=repo_root)
    result = verify_evidence(registry, tests, repo_root=repo_root)
    result["schema_version"] = "llm.mutation-testing.changed-evidence.v1"
    result["enforcement"] = "changed_tests"
    result["checked_test_paths"] = list(tests)
    return result


def _python_test_nodeids(path: Path, relative: str) -> tuple[str, ...]:
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, SyntaxError) as exc:
        raise CampaignError(f"cannot parse test file {relative}: {exc}") from exc
    nodeids: list[str] = []
    for node in tree.body:
        if isinstance(node, ast.FunctionDef) and node.name.startswith("test_"):
            nodeids.append(f"{relative}::{node.name}")
            continue
        if not isinstance(node, ast.ClassDef) or not node.name.startswith("Test"):
            continue
        for child in node.body:
            if isinstance(child, ast.FunctionDef) and child.name.startswith("test_"):
                nodeids.append(f"{relative}::{node.name}::{child.name}")
    if not nodeids:
        raise CampaignError(f"{relative} contains no test functions to rank")
    return tuple(nodeids)


def scaffold_campaign(
    test_path: str,
    *,
    sources: Sequence[str] = (),
    output_path: Path | None = None,
    repo_root: Path = REPO_ROOT,
) -> dict[str, Any]:
    """Write a NOT_READY campaign stub bound to the current file hashes.

    The stub ranks discovered tests and records planned mutation slots. A human
    still has to review first-order patches; this function never generates them.
    """

    relative = _safe_relative_path(test_path.replace("\\", "/"), "scaffold test path")
    target = repo_root / relative
    if not target.is_file():
        raise CampaignError(f"scaffold test path does not exist: {relative}")
    nodeids = (
        _python_test_nodeids(target, relative)
        if relative.endswith(".py")
        else (relative,)
    )
    payload = json.loads(
        _native_or_campaign(
            plan_mutation_scaffold_native,
            str(repo_root),
            relative,
            list(sources),
            list(nodeids),
        )
    )
    campaign_id = payload["campaign_id"].removesuffix("_scaffold")
    destination = output_path or (
        repo_root / "conductor/mutation_campaigns" / f"{campaign_id}_scaffold.json"
    )
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(
        json.dumps(payload, indent=2, sort_keys=False) + "\n", encoding="utf-8"
    )
    manifest_rel = destination.resolve().relative_to(repo_root.resolve()).as_posix()
    patches_dir = f"conductor/mutation_campaigns/patches/{payload['campaign_id']}"
    receipt_rel = f"conductor/mutation_campaigns/receipts/{payload['campaign_id']}_<YYYYMMDD>.json"
    return {
        "status": "NOT_READY",
        "manifest": manifest_rel,
        "campaign_id": payload["campaign_id"],
        "ranked_tests": len(payload["ranked_tests"]),
        "suggested_governance_claim": (
            'make governance-claim OWNER="<your-hook-owner>" CLAIM_PATHS="'
            f'{manifest_rel} {patches_dir}/<mutant>.patch {receipt_rel}" '
            'CLAIM_JUSTIFICATION="<agent> <batch>: mutation coverage for '
            f'{relative}"  # one entry PER patch file; date the receipt path'
        ),
        "next_steps": [
            "Review and replace placeholder contracts, then add one first-order patch.",
            "Register the manifest in conductor/mutation_campaigns/registry.json.",
            "Run make mutation-run; mutation runs are pre-approved (Tim, 2026-08-31).",
            "Keep the PASS receipt under conductor/mutation_campaigns/receipts/.",
        ],
    }


def _json_print(payload: Mapping[str, Any]) -> None:
    print(json.dumps(payload, indent=2, sort_keys=True))


def main(argv: list[str] | None = None) -> int:
    """CLI for coverage inventory, changed-test evidence, and scaffolding."""

    parser = argparse.ArgumentParser(description=__doc__)
    shared = argparse.ArgumentParser(add_help=False)
    shared.add_argument(
        "--registry",
        type=Path,
        default=DEFAULT_REGISTRY,
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser(
        "coverage",
        parents=[shared],
        help="inventory every test file against receipts",
    )
    subparsers.add_parser(
        "changed",
        parents=[shared],
        help="verify PASS receipts for git-changed and untracked tests",
    )
    scaffold = subparsers.add_parser(
        "scaffold",
        parents=[shared],
        help="write a NOT_READY campaign stub; does not generate mutants",
    )
    scaffold.add_argument("test_path")
    scaffold.add_argument(
        "--source",
        action="append",
        default=[],
        help="production source to bind by SHA-256 (repeatable)",
    )
    scaffold.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "coverage":
            result = coverage_report(args.registry)
            _json_print(result)
            return 0 if result["status"] == "PASS" else 5
        if args.command == "changed":
            result = verify_changed(args.registry)
            _json_print(result)
            return 0 if result["status"] == "PASS" else 5
        result = scaffold_campaign(
            args.test_path, sources=args.source, output_path=args.output
        )
        _json_print(result)
        return 0
    except CampaignError as exc:
        _json_print({"status": "REFUSED", "error": str(exc)})
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
