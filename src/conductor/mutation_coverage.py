"""Inventory and changed-test evidence for mutation testing.

This module discovers test files and checks them against registered receipts. It
neither generates mutants nor runs campaigns, and it no longer scaffolds one: the
``scaffold`` subcommand wrote a hand-authored campaign stub whose next step was
"add one first-order patch", which is the manual process KB-MUT-02 forbids. It
was removed on 2026-09-08. Campaigns come from
``conductor.mutation_campaign_generate``, scoped to the files the branch changed.

``coverage`` inventories the whole tree. That is a maintenance report, not the
agent path -- an agent verifies its own change with ``make mutation-evidence``.
``changed --base <ref>`` is the CI shape: the tests a PR changed, checked against
current receipts, with every rejection classified so the exit code can tell debt
(a campaign that never ran, a held ratchet, a receipt from an older runner era)
from a defect (a receipt the validator cannot decode or refuses to accept).
``canary`` is the repo-wide backstop: it ignores how much evidence is missing and
fails only when some receipt anywhere is unreadable.
"""

from __future__ import annotations

import argparse
import json
import os
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path, PurePosixPath
from typing import Any

from conductor._native import (
    is_mutation_test_path_native,
    mutation_rust_test_surface_native,
    mutation_test_inventory_native,
    normalize_mutation_path_native,
    should_skip_mutation_path_native,
)

from conductor.mutation_testing import (
    CANONICAL_TEST_PATTERNS,
    REPO_ROOT,
    CampaignError,
    verify_evidence,
)
from conductor.project_paths import DEFAULT_MUTATION_REGISTRY, host_root
from conductor.project_paths import registry_path as host_registry_path
from conductor.project_paths import registry_relative

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
COVERAGE_SCHEMA = "llm.mutation-testing.coverage.v2"
CHANGED_SCHEMA = "llm.mutation-testing.changed-evidence.v2"
DEFAULT_REGISTRY = Path(DEFAULT_MUTATION_REGISTRY)
# A rejection the validator itself could not get through: a receipt whose detail
# does not decode, a shape it refuses, a manifest that will not load. Everything
# else -- no campaign, a held ratchet, a superseded pointer, an older runner
# era -- is debt for the PR body, and the exit codes keep the two apart.
VALIDATOR_SIDE_KINDS = frozenset(
    {"decode_error", "schema_error", "manifest_load_error"}
)


def _native_or_campaign[T](operation: Callable[..., T], *args: object) -> T:
    try:
        return operation(*args)
    except ValueError as exc:
        raise CampaignError(str(exc)) from exc


def _safe_relative_path(value: str, label: str) -> str:
    return _native_or_campaign(normalize_mutation_path_native, value, label)


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

    registry = registry_path or host_registry_path(repo_root)
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
    base: str | None = None,
) -> tuple[str, ...]:
    """Return mutation-eligible tests that differ from a base ref, untracked included.

    ``base=None`` keeps the local shape (``git diff HEAD``); a named base uses
    merge-base semantics, which is what CI needs: a clean checkout has no
    working-tree diff at all, so the diff has to come from the ref.
    """

    registry = registry_path or host_registry_path(repo_root)
    return tuple(
        _native_or_campaign(
            mutation_test_inventory_native,
            str(repo_root),
            str(registry),
            list(CANONICAL_TEST_PATTERNS),
            sorted(SKIP_DIRECTORY_NAMES),
            "changed" if base is None else "changed-from",
            True,
            base,
        )
    )


def coverage_report(
    registry_path: Path | None = None,
    *,
    repo_root: Path = REPO_ROOT,
    include_untracked: bool = True,
) -> dict[str, Any]:
    """Check every discovered test file against current PASS receipts."""

    registry = registry_path or host_registry_path(repo_root)
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
        "rejection_counts": result["rejection_counts"],
        "malformed_receipts": result.get("malformed_receipts", []),
    }


def verify_changed(
    registry_path: Path | None = None,
    *,
    repo_root: Path = REPO_ROOT,
    base: str | None = None,
) -> dict[str, Any]:
    """Require PASS receipts for changed test files (a base ref, or HEAD)."""

    registry = registry_path or host_registry_path(repo_root)
    tests = git_changed_test_paths(registry, repo_root=repo_root, base=base)
    result = verify_evidence(registry, tests, repo_root=repo_root)
    result["schema_version"] = CHANGED_SCHEMA
    result["enforcement"] = "changed_tests"
    result["checked_test_paths"] = list(tests)
    return result


def _json_print(payload: Mapping[str, Any]) -> None:
    print(json.dumps(payload, indent=2, sort_keys=True))


def _observed_kinds(result: Mapping[str, Any]) -> set[str]:
    """Every rejection kind the result observed: per-row and aggregate.

    The aggregate alone would miss nothing the rows carry, but reading both
    keeps the decision honest when a result was recorded without rows.
    """

    kinds = {
        kind for kind, count in result.get("rejection_counts", {}).items() if count
    }
    for row in result.get("missing_evidence", []):
        kinds.add(row.get("reason_kind", "not_pass"))
        for rejection in row.get("receipt_rejections", []):
            kinds.add(rejection.get("kind", "schema_error"))
    return kinds


def evidence_exit_code(result: Mapping[str, Any]) -> int:
    """0 evidence everywhere, 6 debt only, 5 anything validator-side, never silent."""

    if _observed_kinds(result) & VALIDATOR_SIDE_KINDS:
        return 5
    if result.get("missing_evidence"):
        return 6
    return 0


def _github_output(result: Mapping[str, Any]) -> None:
    """Annotations per missing path and per validator-side rejection, plus a table.

    Workflow commands go to stdout wherever in the log they appear; the table
    lands in ``$GITHUB_STEP_SUMMARY`` when Actions set it, and nowhere else.
    """

    rows = []
    for row in result.get("missing_evidence", []):
        print(f"::warning file={row['path']}::{row['reason']}")
        rejection_kinds = []
        for rejection in row.get("receipt_rejections", []):
            kind = rejection.get("kind")
            if kind not in rejection_kinds:
                rejection_kinds.append(kind)
            if kind in VALIDATOR_SIDE_KINDS:
                print(
                    f"::error::{row['path']}: {rejection['receipt']}: "
                    f"{rejection['detail']}"
                )
        kind = row.get("reason_kind", "not_pass")
        if rejection_kinds:
            kind = f"{kind} ({', '.join(sorted(rejection_kinds))})"
        rows.append(
            (
                row["path"],
                ", ".join(row.get("campaigns", [])) or "—",
                row["reason"],
                kind,
            )
        )
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not summary_path or not rows:
        return
    lines = [
        "",
        "### Mutation evidence (changed tests)",
        "",
        "| path | campaign | status | kind |",
        "|---|---|---|---|",
    ]
    for path, campaigns, status, kind in rows:
        lines.append(f"| `{path}` | {campaigns} | {status} | {kind} |")
    with open(summary_path, "a", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")


def canary_report(
    registry_path: Path | None = None,
    *,
    repo_root: Path = REPO_ROOT,
) -> dict[str, Any]:
    """Repo-wide decode canary: how much evidence is missing is not the question.

    The coverage report counts debt like any other maintenance report; the
    canary asks one thing of it -- can the validator read every receipt on
    disk? A decode, schema or manifest-load rejection anywhere fails it,
    whoever's tests the receipt covers.
    """

    report = coverage_report(registry_path, repo_root=repo_root)
    counts = report["rejection_counts"]
    offending = sorted(kind for kind in VALIDATOR_SIDE_KINDS if counts.get(kind))
    offenders = []
    for row in report.get("missing_evidence", []):
        for rejection in row.get("receipt_rejections", []):
            if rejection.get("kind") in VALIDATOR_SIDE_KINDS:
                offenders.append(
                    {
                        "path": row["path"],
                        "receipt": rejection["receipt"],
                        "kind": rejection["kind"],
                        "detail": rejection["detail"],
                    }
                )
    offenders.extend(
        {"receipt": malformed, "kind": "decode_error", "detail": malformed}
        for malformed in report.get("malformed_receipts", [])
    )
    report["canary"] = {
        "status": "FAIL" if offending else "PASS",
        "offending_kinds": offending,
        "offending_receipts": offenders,
    }
    return report


def main(argv: list[str] | None = None) -> int:
    """CLI for coverage inventory, changed-test evidence and the decode canary."""

    parser = argparse.ArgumentParser(description=__doc__)
    shared = argparse.ArgumentParser(add_help=False)
    shared.add_argument(
        "--registry",
        type=Path,
        default=Path(registry_relative(host_root())),
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser(
        "coverage",
        parents=[shared],
        help="inventory every test file against receipts",
    )
    changed = subparsers.add_parser(
        "changed",
        parents=[shared],
        help="verify PASS receipts for changed tests (a base ref, or HEAD)",
    )
    changed.add_argument(
        "--base",
        default=None,
        help="diff against this ref with merge-base semantics instead of HEAD",
    )
    changed.add_argument(
        "--github",
        action="store_true",
        help="emit workflow annotations and a step-summary table",
    )
    subparsers.add_parser(
        "canary",
        parents=[shared],
        help="fail only when some receipt anywhere is unreadable",
    )
    args = parser.parse_args(argv)
    try:
        if args.command == "coverage":
            result = coverage_report(args.registry)
            _json_print(result)
            return 0 if result["status"] == "PASS" else 5
        if args.command == "canary":
            report = canary_report(args.registry)
            _json_print(report)
            return 0 if report["canary"]["status"] == "PASS" else 5
        result = verify_changed(args.registry, base=args.base)
        if args.github:
            _github_output(result)
        _json_print(result)
        return evidence_exit_code(result)
    except CampaignError as exc:
        _json_print({"status": "REFUSED", "error": str(exc)})
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
