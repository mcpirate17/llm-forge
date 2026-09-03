"""Reproducible latency benchmarks using only temporary repositories and indexes."""

from __future__ import annotations

import argparse
import json
import os
import platform
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Sequence

from conductor.candidate_review.model import write_json_atomic
from conductor.candidate_review.ownership import create_claim
from conductor.candidate_review.policy_path import DEFAULT_POLICY_RELATIVE

SCENARIOS = (
    "docs-only",
    "small-python",
    "native-code",
    "dependency",
    "large-delete-rename",
    "full-review",
)
GOVERNANCE_PATHS = (
    ".github/CODEOWNERS",
    ".github/workflows/governance-ci.yml",
    ".gitignore",
    ".pre-commit-config.yaml",
    "AGENTS.md",
    "Makefile",
    "conductor/_native.py",
    "conductor/_project_hooks.py",
    DEFAULT_POLICY_RELATIVE.as_posix(),
    "conductor/check_duplicate_function_bodies.py",
    "conductor/check_protected_deletes.py",
    "conductor/guardrail_audit.py",
    "conductor/run_duplicate_audit.py",
    "conductor/test_candidate_review.py",
    "conductor/test_candidate_review_cli_policy.py",
    "conductor/test_guardrail_audit.py",
    "conductor/test_ref_aware_governance.py",
    "conductor/test_run_duplicate_audit.py",
    "conductor/test_vulture_audit.py",
    "conductor/tooling_boundary.py",
    "conductor/vulture_baseline.json",
    "research/notes/unified_candidate_review_architecture_2026-08-16.md",
)
HOOK_TREES = (".agent_hooks", ".claude/hooks", "tooling/hooks")
BENCHMARK_CLAIM_PATHS = (
    "benchmark_fixture",
    "package-lock.json",
    "conductor/candidate_review",
    "conductor/check_duplicate_function_bodies.py",
    "conductor/check_protected_deletes.py",
    "conductor/guardrail_audit.py",
    "conductor/run_duplicate_audit.py",
    "conductor/test_candidate_review.py",
    "conductor/test_candidate_review_cli_policy.py",
    "conductor/test_guardrail_audit.py",
    "conductor/test_ref_aware_governance.py",
    "conductor/test_run_duplicate_audit.py",
    "conductor/test_vulture_audit.py",
)


class BenchmarkError(RuntimeError):
    """A benchmark fixture or review did not produce complete evidence."""


@dataclass(frozen=True, slots=True)
class Fixture:
    source: Path
    repo: Path
    baseline_commit: str
    baseline_tree: str


def _run(
    command: Sequence[str],
    *,
    cwd: Path,
    environment: dict[str, str] | None = None,
    input_bytes: bytes | None = None,
) -> subprocess.CompletedProcess[bytes]:
    completed = subprocess.run(
        list(command),
        cwd=cwd,
        env=environment,
        input=input_bytes,
        capture_output=True,
        check=False,
    )
    if completed.returncode:
        detail = (completed.stderr or completed.stdout).decode("utf-8", "replace")
        raise BenchmarkError(f"{' '.join(command)} failed: {detail.strip()}")
    return completed


def _git(
    repo: Path,
    arguments: Sequence[str],
    *,
    index: Path | None = None,
    input_bytes: bytes | None = None,
) -> str:
    environment = os.environ.copy()
    if index is not None:
        environment["GIT_INDEX_FILE"] = str(index)
    completed = _run(
        ["git", *arguments],
        cwd=repo,
        environment=environment,
        input_bytes=input_bytes,
    )
    return completed.stdout.decode("utf-8", "replace").strip()


def _insert(index: Path, repo: Path, path: str, content: bytes, mode: str) -> str:
    oid = _git(repo, ["hash-object", "-w", "--stdin"], input_bytes=content)
    _git(
        repo,
        ["update-index", "--add", "--cacheinfo", f"{mode},{oid},{path}"],
        index=index,
    )
    return oid


def _source_paths(source: Path) -> list[str]:
    """The governance surface: fixed paths, the review package, and the hook trees.

    The always-on tooling-boundary check refuses a tree with no hook directory next
    to ``conductor/`` (rule b scans it), so every hook tree present at the source is
    carried into the fixture.
    """
    paths: list[str] = list(GOVERNANCE_PATHS)
    paths.extend(
        path.relative_to(source).as_posix()
        for path in sorted((source / "conductor" / "candidate_review").glob("*.py"))
    )
    for hook_tree in HOOK_TREES:
        paths.extend(
            path.relative_to(source).as_posix()
            for path in sorted((source / hook_tree).rglob("*"))
            if path.is_file() and "__pycache__" not in path.parts
        )
    return sorted(set(paths))


def _prepare_fixture(source: Path, root: Path) -> Fixture:
    repo = root / "repo"
    _run(
        [
            "git",
            "clone",
            "--quiet",
            "--shared",
            "--no-checkout",
            str(source),
            str(repo),
        ],
        cwd=root,
    )
    index = root / "baseline.index"
    _git(repo, ["read-tree", "HEAD"], index=index)
    for relative in _source_paths(source):
        path = source / relative
        if not path.is_file():
            raise BenchmarkError(f"required benchmark source is missing: {relative}")
        content = path.read_bytes()
        mode = "100755" if os.access(path, os.X_OK) else "100644"
        _insert(index, repo, relative, content, mode)
    dummy = b"retained benchmark fixture\n"
    for number in range(500):
        _insert(
            index,
            repo,
            f"benchmark_fixture/obsolete_{number:04d}.txt",
            dummy,
            "100644",
        )
    baseline_tree = _git(repo, ["write-tree"], index=index)
    commit_environment = os.environ.copy()
    commit_environment.update(
        {
            "GIT_AUTHOR_NAME": "candidate-review-benchmark",
            "GIT_AUTHOR_EMAIL": "benchmark@example.invalid",
            "GIT_COMMITTER_NAME": "candidate-review-benchmark",
            "GIT_COMMITTER_EMAIL": "benchmark@example.invalid",
        }
    )
    baseline_commit = (
        _run(
            ["git", "commit-tree", baseline_tree, "-p", "HEAD"],
            cwd=repo,
            environment=commit_environment,
            input_bytes=b"candidate review benchmark baseline\n",
        )
        .stdout.decode()
        .strip()
    )
    create_claim(
        repo,
        owner="Codex",
        paths=BENCHMARK_CLAIM_PATHS,
        justification="isolated candidate-review latency benchmark",
        max_minutes=60,
    )
    return Fixture(source, repo, baseline_commit, baseline_tree)


def _scenario_index(fixture: Fixture, root: Path, scenario: str) -> Path:
    index = root / f"{scenario}.index"
    _git(fixture.repo, ["read-tree", fixture.baseline_tree], index=index)
    if scenario == "docs-only":
        _insert(
            index,
            fixture.repo,
            "research/notes/candidate_review_benchmark_probe.md",
            b"# Candidate review benchmark\n\nExact docs-only latency probe.\n",
            "100644",
        )
    elif scenario in {"small-python", "full-review"}:
        relative = "conductor/test_candidate_review.py"
        content = (fixture.source / relative).read_bytes()
        changed = content.replace(
            b"candidate-bound governance review.",
            b"candidate-bound governance review benchmark.",
            1,
        )
        if changed == content:
            raise BenchmarkError("small Python benchmark marker was not found")
        _insert(index, fixture.repo, relative, changed, "100644")
    elif scenario == "native-code":
        _insert(
            index,
            fixture.repo,
            "conductor/candidate_review/test_benchmark_probe.c",
            b"#include <stdint.h>\nint64_t benchmark_identity(int64_t x) { return x; }\n",
            "100644",
        )
    elif scenario == "dependency":
        relative = "package-lock.json"
        content = (fixture.source / relative).read_bytes()
        _insert(index, fixture.repo, relative, content + b"\n", "100644")
    elif scenario == "large-delete-rename":
        for number in range(250):
            old = f"benchmark_fixture/obsolete_{number:04d}.txt"
            _git(fixture.repo, ["update-index", "--force-remove", old], index=index)
        blob = _git(
            fixture.repo,
            [
                "rev-parse",
                f"{fixture.baseline_tree}:benchmark_fixture/obsolete_0250.txt",
            ],
        )
        for number in range(250, 500):
            old = f"benchmark_fixture/obsolete_{number:04d}.txt"
            new = f"benchmark_fixture/renamed_{number:04d}.txt"
            _git(fixture.repo, ["update-index", "--force-remove", old], index=index)
            _git(
                fixture.repo,
                ["update-index", "--add", "--cacheinfo", f"100644,{blob},{new}"],
                index=index,
            )
    else:
        raise BenchmarkError(f"unknown benchmark scenario: {scenario}")
    return index


def _review_once(
    fixture: Fixture,
    root: Path,
    scenario: str,
    index: Path,
    run_name: str,
) -> dict[str, object]:
    profile = "full" if scenario == "full-review" else "fast"
    receipt = root / f"{scenario}-{run_name}.json"
    timing = root / f"{scenario}-{run_name}.time"
    environment = os.environ.copy()
    environment.update(
        {
            "GIT_INDEX_FILE": str(index),
            "GOVERNANCE_OWNER": "Codex",
            "PATH": (
                f"{fixture.source / 'node_modules' / '.bin'}"
                f"{os.pathsep}{environment.get('PATH', '')}"
            ),
        }
    )
    command = [
        "/usr/bin/time",
        "-f",
        "%e %M",
        "-o",
        str(timing),
        sys.executable,
        "-m",
        "conductor.candidate_review.cli",
        "review",
        "--repo",
        str(fixture.repo),
        "--surface",
        "pre-commit",
        "--candidate",
        "index",
        "--base-ref",
        fixture.baseline_commit,
        "--profile",
        profile,
        "--json-out",
        str(receipt),
    ]
    started = time.perf_counter()
    completed = subprocess.run(
        command,
        cwd=fixture.source,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    wall_fallback = time.perf_counter() - started
    if not receipt.is_file() or not timing.is_file():
        raise BenchmarkError(
            f"{scenario}/{run_name} produced incomplete evidence: "
            f"exit={completed.returncode} stderr={completed.stderr[-2000:]}"
        )
    timing_lines = [
        line.split()
        for line in timing.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    measurements = [parts for parts in timing_lines if len(parts) == 2]
    if not measurements:
        raise BenchmarkError(f"{scenario}/{run_name} has malformed time evidence")
    wall_raw, rss_raw = measurements[-1]
    payload = json.loads(receipt.read_text(encoding="utf-8"))
    return {
        "run": run_name,
        "profile": profile,
        "exit_code": completed.returncode,
        "decision": payload.get("decision"),
        "wall_seconds": float(wall_raw) if wall_raw else round(wall_fallback, 3),
        "max_rss_kib": int(rss_raw),
        "engine_duration_ms": payload.get("timings", {}).get("duration_ms"),
        "cache": payload.get("cache"),
        "finding_counts": _finding_counts(payload.get("findings", [])),
        "findings": _finding_summary(payload.get("findings", [])),
        "receipt_id": payload.get("receipt_id"),
        "tree_oid": payload.get("candidate", {}).get("tree_oid"),
    }


def _finding_counts(findings: object) -> dict[str, int]:
    counts: dict[str, int] = {}
    if not isinstance(findings, list):
        return counts
    for finding in findings:
        if isinstance(finding, dict):
            severity = str(finding.get("severity", "unknown"))
            counts[severity] = counts.get(severity, 0) + 1
    return counts


def _finding_summary(findings: object) -> list[dict[str, object]]:
    if not isinstance(findings, list):
        return []
    return [
        {
            "severity": finding.get("severity"),
            "check_id": finding.get("check_id"),
            "rule_id": finding.get("rule_id"),
            "path": finding.get("path"),
            "message": str(finding.get("message", ""))[:2000],
        }
        for finding in findings
        if isinstance(finding, dict)
    ]


def run_benchmarks(source: Path, scenarios: Sequence[str]) -> dict[str, object]:
    with tempfile.TemporaryDirectory(prefix="llm-candidate-benchmark-") as raw:
        root = Path(raw)
        fixture = _prepare_fixture(source, root)
        results: dict[str, list[dict[str, object]]] = {}
        for scenario in scenarios:
            index = _scenario_index(fixture, root, scenario)
            results[scenario] = [
                _review_once(fixture, root, scenario, index, "cold"),
                _review_once(fixture, root, scenario, index, "warm"),
            ]
        return {
            "schema_version": 1,
            "measured_at": datetime.now().astimezone().isoformat(),
            "source_head": _git(source, ["rev-parse", "HEAD"]),
            "source_tree": _git(source, ["rev-parse", "HEAD^{tree}"]),
            "benchmark_baseline_commit": fixture.baseline_commit,
            "benchmark_baseline_tree": fixture.baseline_tree,
            "host": {
                "platform": platform.platform(),
                "python": platform.python_version(),
                "processor": platform.processor(),
                "cpu_count": os.cpu_count(),
            },
            "scenarios": results,
        }


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=".")
    parser.add_argument(
        "--output", default="tasks/audit/candidate_review_latency_2026-08-16.json"
    )
    parser.add_argument("--scenario", action="append", choices=SCENARIOS)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    source = Path(args.repo).resolve()
    scenarios = tuple(args.scenario or SCENARIOS)
    payload = run_benchmarks(source, scenarios)
    output = Path(args.output)
    if not output.is_absolute():
        output = source / output
    write_json_atomic(output, payload)
    print(json.dumps(payload, indent=2, sort_keys=True))
    print(f"benchmark receipt: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
