"""Inventory, changed-test evidence, and campaign scaffolding for mutation testing.

The runner in ``conductor.mutation_testing`` stays the receipt authority. This
module discovers test files, checks them against registered PASS receipts, and
scaffolds a campaign stub. It does not generate mutants or run campaigns.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import subprocess
from pathlib import Path, PurePosixPath
from typing import Any, Mapping, Sequence

from conductor.mutation_testing import CampaignError, REPO_ROOT, verify_evidence


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


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    digest.update(path.read_bytes())
    return digest.hexdigest()


def _safe_relative_path(value: str, label: str) -> str:
    text = value.replace("\\", "/")
    path = PurePosixPath(text)
    if path.is_absolute() or ".." in path.parts or text.startswith("./"):
        raise CampaignError(f"{label} must be a normalized repository-relative path")
    if not text.strip():
        raise CampaignError(f"{label} must be a non-empty string")
    return path.as_posix()


def _registry_patterns(registry_path: Path, repo_root: Path) -> tuple[str, ...]:
    resolved = registry_path.resolve()
    try:
        resolved.relative_to(repo_root.resolve())
    except ValueError as exc:
        raise CampaignError("mutation registry must be inside the repository") from exc
    try:
        payload = json.loads(resolved.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise CampaignError(f"cannot load mutation registry {resolved}: {exc}") from exc
    if not isinstance(payload, dict):
        raise CampaignError("registry must be a JSON object")
    patterns = payload.get("test_patterns")
    if (
        not isinstance(patterns, list)
        or not patterns
        or not all(isinstance(item, str) and item for item in patterns)
    ):
        raise CampaignError(
            "registry.test_patterns must be a list of non-empty strings"
        )
    return tuple(patterns)


def is_test_path(path: str, patterns: Sequence[str]) -> bool:
    """Return whether a repository-relative path matches mutation test patterns."""

    candidate = PurePosixPath(path.replace("\\", "/"))
    return any(candidate.match(pattern) for pattern in patterns)


def _git_paths(repo_root: Path, args: Sequence[str]) -> tuple[str, ...]:
    completed = subprocess.run(
        ["git", *args],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise CampaignError(f"git {' '.join(args)} failed: {detail}")
    return tuple(
        line.replace("\\", "/")
        for line in completed.stdout.splitlines()
        if line.strip()
    )


def _should_skip(relative: PurePosixPath) -> bool:
    parts = relative.parts
    if any(part in SKIP_DIRECTORY_NAMES for part in parts):
        return True
    return "research" in parts and "cache" in parts


def discover_test_paths(
    registry_path: Path | None = None,
    *,
    repo_root: Path = REPO_ROOT,
    include_untracked: bool = True,
) -> tuple[str, ...]:
    """Return git-visible test files matching the mutation registry patterns."""

    registry = registry_path or (repo_root / DEFAULT_REGISTRY)
    patterns = _registry_patterns(registry, repo_root)
    tracked = _git_paths(repo_root, ["ls-files"])
    untracked: tuple[str, ...] = ()
    if include_untracked:
        untracked = _git_paths(
            repo_root, ["ls-files", "--others", "--exclude-standard"]
        )
    discovered: list[str] = []
    seen: set[str] = set()
    for raw in (*tracked, *untracked):
        posix = PurePosixPath(raw.replace("\\", "/"))
        path = posix.as_posix()
        if path in seen or _should_skip(posix):
            continue
        if not is_test_path(path, patterns):
            continue
        seen.add(path)
        discovered.append(path)
    return tuple(sorted(discovered))


def git_changed_test_paths(
    registry_path: Path | None = None,
    *,
    repo_root: Path = REPO_ROOT,
) -> tuple[str, ...]:
    """Return mutation-eligible tests that differ from HEAD, including untracked."""

    registry = registry_path or (repo_root / DEFAULT_REGISTRY)
    patterns = _registry_patterns(registry, repo_root)
    names = {
        *_git_paths(repo_root, ["diff", "--name-only", "HEAD"]),
        *_git_paths(repo_root, ["ls-files", "--others", "--exclude-standard"]),
    }
    selected: list[str] = []
    for raw in names:
        path = raw.replace("\\", "/")
        posix = PurePosixPath(path)
        if _should_skip(posix) or not is_test_path(path, patterns):
            continue
        selected.append(path)
    return tuple(sorted(selected))


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
    source_sha256: dict[str, str] = {relative: _sha256(target)}
    for raw_source in sources:
        source_rel = _safe_relative_path(
            raw_source.replace("\\", "/"), "scaffold source path"
        )
        source_file = repo_root / source_rel
        if not source_file.is_file():
            raise CampaignError(f"scaffold source path does not exist: {source_rel}")
        source_sha256[source_rel] = _sha256(source_file)
    nodeids = (
        _python_test_nodeids(target, relative)
        if relative.endswith(".py")
        else (relative,)
    )
    ranked_tests = [
        {
            "rank": index,
            "nodeid": nodeid,
            "contract": "replace with the behavioral contract this test enforces",
            "rationale": "rank by the damage a silent defect would do",
        }
        for index, nodeid in enumerate(nodeids, start=1)
    ]
    campaign_id = PurePosixPath(relative).stem
    planned_target = next(
        (path for path in source_sha256 if path != relative), relative
    )
    payload: dict[str, Any] = {
        "schema_version": 1,
        "campaign_id": f"{campaign_id}_scaffold",
        "title": f"Scaffolded campaign for {relative}",
        "language": "python" if relative.endswith(".py") else "unknown",
        "mutation_engine": "reviewed_unified_diff",
        "expected_ranked_tests": len(ranked_tests),
        "expected_mutations": 1,
        "source_sha256": source_sha256,
        "ranked_tests": ranked_tests,
        "planned_mutations": [
            {
                "id": "first_order_placeholder",
                "target_path": planned_target,
                "contract": "replace with the first-order defect this test must kill",
                "description": (
                    "Materialize one reviewed unified diff. Do not generate mutants "
                    "automatically and do not edit the shared checkout."
                ),
                "expected_killers": [ranked_tests[0]["nodeid"]],
            }
        ],
        "mutations": [],
        "baseline": {
            "argv": ["python", "-m", "pytest", "-q", "-o", "addopts=", *nodeids],
            "timeout_seconds": 120,
        },
        "resource_gate": {
            "blocked_process_substrings": [],
            "poll_seconds": 30,
        },
        "environment": {},
        "host_read_dependencies": [],
    }
    destination = output_path or (
        repo_root / "conductor/mutation_campaigns" / f"{campaign_id}_scaffold.json"
    )
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(
        json.dumps(payload, indent=2, sort_keys=False) + "\n", encoding="utf-8"
    )
    return {
        "status": "NOT_READY",
        "manifest": destination.resolve().relative_to(repo_root.resolve()).as_posix(),
        "campaign_id": payload["campaign_id"],
        "ranked_tests": len(ranked_tests),
        "next_steps": [
            "Review and replace placeholder contracts, then add one first-order patch.",
            "Register the manifest in conductor/mutation_campaigns/registry.json.",
            "Obtain Tim's explicit authority, then make mutation-run.",
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
