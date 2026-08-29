"""Focused integration tests for candidate-bound governance review."""

from __future__ import annotations

import copy
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import xml.etree.ElementTree as ET
from dataclasses import replace
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any, cast

import pytest

from conductor.candidate_review import checks as review_checks
from conductor.candidate_review import engine as review_engine
from conductor.candidate_review import verification as review_verification
from conductor.candidate_review.checks import (
    ReviewContext,
    TestSelection as ReviewTestSelection,
    check_candidate_integrity,
    check_config_and_notebooks,
    check_dependency_integrity,
    check_duplicate_function_bodies,
    check_native_source,
    check_ownership,
    check_performance_evidence,
    check_python_ast,
    check_research_evidence,
    check_mutation_evidence,
    check_secrets,
    files_for_policy,
    run_builtin,
)
from conductor.candidate_review.command_runner import (
    _environment,
    command_cache_material,
    prepare_candidate_git_environment,
    run_command_check,
    tool_version,
)
from conductor.candidate_review.engine import (
    ResultCache,
    ReviewOutcome,
    append_attestation,
    governance_lock,
    receipt_path,
    run_locked_git_commit,
    run_review,
    verify_receipt_payload,
)
from conductor.candidate_review.git_source import (
    GitSourceError,
    classify_candidate,
    list_tree,
    materialize_tree,
    resolve_candidate,
    resolve_commit,
)
from conductor.candidate_review.model import (
    Candidate,
    Change,
    CheckResult,
    CheckStatus,
    Finding,
    ReviewReceipt,
    Severity,
    TreeEntry,
    seal_receipt,
    write_json_atomic,
)
from conductor.candidate_review.ownership import (
    OwnershipError,
    claim_store_path,
    create_claim,
    load_claims,
    release_claim,
)
from conductor.candidate_review.policy import PolicyError, load_policy
from conductor.candidate_review.reporters import (
    human_summary,
    junit_xml,
    sarif_payload,
    write_outputs,
)
from conductor.candidate_review.verification import (
    check_test_evidence,
    run_targeted_tests,
)


def _git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", *args],
        cwd=repo,
        capture_output=True,
        check=False,
        text=True,
    )
    if completed.returncode:
        pytest.fail(
            f"git {' '.join(args)} failed ({completed.returncode}): "
            f"{completed.stderr.strip()}"
        )
    return completed.stdout.strip()


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _git(path, "init", "--quiet", "--initial-branch=main")
    _git(path, "config", "user.name", "Candidate Review Test")
    _git(path, "config", "user.email", "candidate-review@example.invalid")
    return path


def _commit_all(repo: Path, message: str) -> str:
    _git(repo, "add", "--all")
    _git(repo, "commit", "--quiet", "--message", message)
    return _git(repo, "rev-parse", "HEAD")


def _install_candidate_engine(repo: Path) -> None:
    source = Path(__file__).parent / "candidate_review"
    target = repo / "conductor" / "candidate_review"
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source, target, ignore=shutil.ignore_patterns("__pycache__"))


def _run_fixture_review(
    repo: Path,
    candidate: Candidate,
    *,
    surface: str,
    runtime_dir: Path,
) -> ReviewOutcome:
    with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
        policy = load_policy(snapshot / "conductor" / "candidate_policy.toml")
        classified = classify_candidate(candidate, policy)
        return run_review(
            ReviewContext(
                repo=repo,
                snapshot=snapshot,
                candidate=classified,
                entries=entries,
                policy=policy,
                surface=surface,
                profile="fast",
                owner=None,
                runtime_dir=runtime_dir,
            )
        )


def _minimal_policy_text(
    *,
    baseline_expires: str,
    exceptions: str = "exceptions = []",
) -> str:
    return f"""\
schema_version = 1
block_at = "high"
max_workers = 1
cache_ttl_days = 1
claim_max_age_hours = 1
max_file_bytes = 1000000
max_binary_bytes = 1000000
coverage_threshold = 75.0
high_risk_coverage_threshold = 90.0
baseline_expires = {baseline_expires}
{exceptions}

[classes]

[risk]
high = []

[paths]
protected_deletes = []
hot = []
generated = []

[checks.candidate-integrity]
kind = "builtin"
profiles = ["fast", "full"]
classes = []
severity = "critical"
always = true
cache = false
run_on_deletions = true
timeout_seconds = 10
memory_mb = 128
max_output_chars = 1000
"""


def _receipt() -> ReviewReceipt:
    finding = {
        "check_id": "candidate-integrity",
        "rule_id": "unsafe-symlink",
        "severity": "critical",
        "message": "candidate symlink escapes its snapshot",
        "path": "escape",
        "line": 2,
        "column": 0,
        "help": "use a repository-relative target",
        "evidence": {},
        "fingerprint": "test-fingerprint-not-a-secret",
        "exception_id": None,
    }
    return seal_receipt(
        ReviewReceipt(
            schema_version=1,
            receipt_id="",
            receipt_digest="",
            surface="manual",
            profile="fast",
            decision="fail",
            candidate={
                "kind": "commit",
                "tree_oid": "a" * 40,
                "base_tree_oid": "b" * 40,
                "base_commit_oid": "c" * 40,
                "commit_oid": "d" * 40,
                "target_ref": "HEAD",
                "changes": [],
            },
            policy={"sha256": "e" * 64},
            engine={"candidate_source_sha256": "f" * 64},
            graph={},
            bypass={},
            timings={"duration_ms": 125},
            cache={"hits": 0, "misses": 1},
            baselines=[],
            checks=[
                {
                    "check_id": "candidate-integrity",
                    "status": "failed",
                    "duration_ms": 125,
                    "findings": [finding.copy()],
                    "stdout_tail": "",
                    "stderr_tail": "bad candidate",
                }
            ],
            findings=[finding.copy()],
            binding="0" * 64,
        )
    )


def test_index_candidate_ignores_unstaged_and_untracked_content(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "tracked.txt").write_text("committed\n", encoding="utf-8")
    _commit_all(repo, "baseline")

    (repo / "candidate.txt").write_text("staged\n", encoding="utf-8")
    _git(repo, "add", "candidate.txt")
    (repo / "tracked.txt").write_text("unstaged\n", encoding="utf-8")
    (repo / "untracked.txt").write_text("untracked\n", encoding="utf-8")

    candidate = resolve_candidate(repo, kind="index")

    assert [(change.status, change.path) for change in candidate.changes] == [
        ("A", "candidate.txt")
    ]
    with materialize_tree(repo, candidate.tree_oid) as (snapshot, _entries):
        assert (snapshot / "candidate.txt").read_text(encoding="utf-8") == "staged\n"
        assert (snapshot / "tracked.txt").read_text(encoding="utf-8") == ("committed\n")
        assert not (snapshot / "untracked.txt").exists()


def test_structured_claims_reject_exact_path_overlap_and_bind_content(
    tmp_path: Path,
) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "base.txt").write_text("base\n", encoding="utf-8")
    _commit_all(repo, "baseline")

    claim = create_claim(
        repo,
        owner="Codex",
        paths=["conductor/candidate_review"],
        justification="candidate governance implementation",
        hours=1,
    )
    claims, digest = load_claims(repo)
    assert claims == (claim,)
    assert len(digest) == 64
    with pytest.raises(OwnershipError, match="overlaps active claim"):
        create_claim(
            repo,
            owner="Other agent",
            paths=["conductor/candidate_review/engine.py"],
            justification="conflicting edit",
            hours=1,
        )
    with pytest.raises(OwnershipError, match="not 'Other agent'"):
        release_claim(repo, claim_id=claim.claim_id, owner="Other agent")
    assert release_claim(repo, claim_id=claim.claim_id, owner="Codex")
    assert load_claims(repo)[0] == ()


def test_ownership_claim_is_independent_of_ignored_worktree_ledger(
    tmp_path: Path,
) -> None:
    repo = _init_repo(tmp_path / "repo")
    policy_path = repo / "conductor" / "candidate_policy.toml"
    policy_path.parent.mkdir(parents=True)
    policy_path.write_text(
        _minimal_policy_text(
            baseline_expires=(datetime.now(timezone.utc) + timedelta(days=30))
            .date()
            .isoformat()
        ),
        encoding="utf-8",
    )
    (repo / "source.py").write_text("VALUE = 1\n", encoding="utf-8")
    _commit_all(repo, "baseline")
    (repo / "source.py").write_text("VALUE = 2\n", encoding="utf-8")
    _git(repo, "add", "source.py")
    candidate = resolve_candidate(repo, kind="index")
    create_claim(
        repo,
        owner="Codex",
        paths=["source.py"],
        justification="focused ownership test",
        hours=1,
    )

    def result_with_ledger(text: str):
        (repo / ".current_work.md").write_text(text, encoding="utf-8")
        with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
            return check_ownership(
                ReviewContext(
                    repo=repo,
                    snapshot=snapshot,
                    candidate=candidate,
                    entries=entries,
                    policy=load_policy(snapshot / "conductor/candidate_policy.toml"),
                    surface="pre-commit",
                    profile="fast",
                    owner="Codex",
                    runtime_dir=tmp_path / "runtime",
                )
            )

    first = result_with_ledger("unrelated untracked state one\n")
    second = result_with_ledger("malicious unrelated state two\n")
    assert first.findings == second.findings == []
    assert first.metrics["state_sha256"] == second.metrics["state_sha256"]

    claim_store_path(repo).write_text("{", encoding="utf-8")
    malformed = result_with_ledger("still unrelated\n")
    assert [finding.rule_id for finding in malformed.findings] == [
        "malformed-claim-store"
    ]


def test_range_candidate_uses_merge_base_even_with_a_clean_index(
    tmp_path: Path,
) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "shared.txt").write_text("base\n", encoding="utf-8")
    merge_base = _commit_all(repo, "base")

    _git(repo, "switch", "--quiet", "--create", "feature")
    (repo / "shared.txt").write_text("feature\n", encoding="utf-8")
    (repo / "feature.txt").write_text("feature-only\n", encoding="utf-8")
    feature_commit = _commit_all(repo, "feature")

    _git(repo, "switch", "--quiet", "main")
    (repo / "shared.txt").write_text("main\n", encoding="utf-8")
    _commit_all(repo, "main divergence")
    assert _git(repo, "status", "--porcelain") == ""

    candidate = resolve_candidate(
        repo,
        kind="range",
        target_ref="feature",
        base_ref="main",
    )

    assert candidate.base_commit_oid == merge_base
    assert candidate.commit_oid == feature_commit
    assert {change.path for change in candidate.changes} == {
        "feature.txt",
        "shared.txt",
    }


def test_ci_empty_range_fails_closed_with_clean_index(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    _install_candidate_engine(repo)
    policy_path = repo / "conductor" / "candidate_policy.toml"
    policy_path.write_text(
        _minimal_policy_text(
            baseline_expires=(datetime.now(timezone.utc) + timedelta(days=30))
            .date()
            .isoformat()
        ),
        encoding="utf-8",
    )
    _commit_all(repo, "governance baseline")
    assert _git(repo, "status", "--porcelain") == ""

    candidate = resolve_candidate(
        repo,
        kind="range",
        base_ref="HEAD",
        target_ref="HEAD",
    )
    outcome = _run_fixture_review(
        repo,
        candidate,
        surface="ci",
        runtime_dir=tmp_path / "runtime",
    )

    assert outcome.receipt.decision == "fail"
    assert any(
        finding.rule_id == "empty-ci-range" and finding.severity.value == "critical"
        for result in outcome.results
        for finding in result.findings
    )
    assert outcome.receipt.candidate["tree_oid"] == candidate.tree_oid
    assert verify_receipt_payload(outcome.receipt.to_dict()) == (True, "ok")


def test_local_and_ci_policy_findings_are_parity_bound(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    _install_candidate_engine(repo)
    policy_path = repo / "conductor" / "candidate_policy.toml"
    policy_path.write_text(
        _minimal_policy_text(
            baseline_expires=(datetime.now(timezone.utc) + timedelta(days=30))
            .date()
            .isoformat()
        ),
        encoding="utf-8",
    )
    base = _commit_all(repo, "governance baseline")
    (repo / "README.md").write_text("candidate docs\n", encoding="utf-8")
    _git(repo, "add", "README.md")
    local_candidate = resolve_candidate(repo, kind="index", base_ref=base)
    local = _run_fixture_review(
        repo,
        local_candidate,
        surface="pre-commit",
        runtime_dir=tmp_path / "local-runtime",
    )
    commit = _commit_all(repo, "candidate")
    ci_candidate = resolve_candidate(
        repo,
        kind="range",
        base_ref=base,
        target_ref=commit,
    )
    ci = _run_fixture_review(
        repo,
        ci_candidate,
        surface="ci",
        runtime_dir=tmp_path / "ci-runtime",
    )

    assert local.receipt.candidate["tree_oid"] == ci.receipt.candidate["tree_oid"]
    assert local.receipt.policy["sha256"] == ci.receipt.policy["sha256"]
    local_findings = {
        (finding.check_id, finding.rule_id, finding.fingerprint)
        for result in local.results
        if result.check_id not in {"engine-integrity", "attestation"}
        for finding in result.findings
    }
    ci_findings = {
        (finding.check_id, finding.rule_id, finding.fingerprint)
        for result in ci.results
        if result.check_id not in {"engine-integrity", "attestation"}
        for finding in result.findings
    }
    assert local_findings == ci_findings
    assert local.receipt.decision == ci.receipt.decision == "pass"


def test_analyzer_git_mutation_cannot_rebind_shared_worktree(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "candidate.txt").write_text("candidate\n", encoding="utf-8")
    _commit_all(repo, "candidate")
    candidate = resolve_candidate(repo, kind="index")

    with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
        context = ReviewContext(
            repo=repo,
            snapshot=snapshot,
            candidate=candidate,
            entries=entries,
            policy=load_policy(
                _write_fixture_policy(repo, datetime.now(timezone.utc).date())
            ),
            surface="pre-commit",
            profile="fast",
            owner=None,
            runtime_dir=tmp_path / "runtime",
        )
        prepare_candidate_git_environment(context)
        completed = subprocess.run(
            ["git", "config", "core.worktree", "/tmp/analyzer-poison"],
            cwd=snapshot,
            env=_environment(context),
            capture_output=True,
            text=True,
            check=False,
        )
        assert completed.returncode == 0, completed.stderr

    shared = subprocess.run(
        ["git", "config", "--local", "--get", "core.worktree"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    assert shared.returncode == 1


def _write_fixture_policy(repo: Path, today: date) -> Path:
    policy_path = repo / "policy.toml"
    policy_path.write_text(
        _minimal_policy_text(baseline_expires=(today + timedelta(days=30)).isoformat()),
        encoding="utf-8",
    )
    return policy_path


def _write_adversarial_sources(repo: Path) -> None:
    (repo / "research" / "data").mkdir(parents=True)
    (repo / "research" / "data" / "protected.txt").write_text(
        "protected\n", encoding="utf-8"
    )
    _commit_all(repo, "protected baseline")
    _git(repo, "rm", "research/data/protected.txt")
    sources = {
        "component_fab/harness/new_mechanism.py": (
            """\
# TODO remove scaffold
import json, pickle, subprocess, yaml
import random
def pending(): pass
def missing(): ...
def unsafe(items):
    for outer in items:
        for inner in items:
            json.loads('{}')
    eval('1')
    pickle.loads(b'x')
    yaml.load('x')
    subprocess.run('x', shell=True)
    raise NotImplementedError()
RANDOM = random"""
            + "."
            + "random()\n"
            + """\
def softmax_fallback(): return 'attention fallback'
# performance-critical pure Python
"""
        ),
        "research/tools/result_record_probe.py": "stage1_passed = True\nscore = 1\ndtype = 'bad'\n",
        "duplicate_a.py": "def copied(value):\n    value += 1\n    value += 2\n    value += 3\n    value += 4\n    value += 5\n    value += 6\n    value += 7\n    value += 8\n    return value\n",
        "duplicate_b.py": "def copied_again(value):\n    value += 1\n    value += 2\n    value += 3\n    value += 4\n    value += 5\n    value += 6\n    value += 7\n    value += 8\n    return value\n",
        "secrets.py": "{} = {!r}\n".format("api_" + "key", "A" * 32),
        "package.json": "{}\n",
        "settings.json": "{\n",
        "research/runtime/native/bad.c": "void bad(char *x) { system(x); strcpy(x, x); }\n",
    }
    for relative, content in sources.items():
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
    notebook = repo / "probe.ipynb"
    notebook.write_text(
        json.dumps(
            {
                "nbformat": 4,
                "cells": [
                    {
                        "cell_type": "code",
                        "outputs": [{"text": "x"}],
                        "execution_count": 1,
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    (repo / "artifact.pt").write_bytes(b"binary")
    _git(repo, "add", "--all")


def test_adversarial_builtin_matrix_exercises_real_candidate_flows(
    tmp_path: Path,
) -> None:
    repo = _init_repo(tmp_path / "repo")
    _write_adversarial_sources(repo)
    policy = replace(
        load_policy(Path("conductor/candidate_policy.toml")),
        max_file_bytes=200,
        max_binary_bytes=1,
    )
    candidate = classify_candidate(resolve_candidate(repo, kind="index"), policy)
    selection = ReviewTestSelection(tests=(), graph={}, findings=())
    with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
        context = ReviewContext(
            repo=repo,
            snapshot=snapshot,
            candidate=candidate,
            entries=entries,
            policy=policy,
            surface="ci",
            profile="full",
            owner=None,
            runtime_dir=tmp_path / "runtime",
        )
        results = [
            check_candidate_integrity(context),
            check_config_and_notebooks(context),
            check_secrets(context),
            check_python_ast(context),
            check_dependency_integrity(context),
            check_performance_evidence(context, selection),
            check_research_evidence(context, selection),
            check_native_source(context),
            check_duplicate_function_bodies(context),
        ]
        rules = {finding.rule_id for result in results for finding in result.findings}
        expected_rules = {
            "protected-delete-or-move",
            "binary-admission",
            "oversized-artifact",
            "malformed-config",
            "notebook-output",
            "generic-api-key",
            "pass-stub",
            "ellipsis-stub",
            "dynamic-execution",
            "unsafe-deserialization",
            "unsafe-yaml",
            "unsafe-shell",
            "not-implemented-stub",
            "softmax-shaped-fallback",
            "nondeterministic-research",
            "partial-promotion-write",
            "missing-lockfile",
            "missing-performance-budget",
            "python-only-hotpath",
            "incomplete-result-provenance",
            "missing-numerical-device-tests",
            "unsafe-native-api",
            "copied-function-body",
        }
        assert expected_rules <= rules, sorted(expected_rules - rules)
        command_check = next(
            check for check in policy.checks if check.kind == "command"
        )
        assert files_for_policy(context, command_check)
        unknown = replace(command_check, check_id="missing-builtin", kind="builtin")
        assert run_builtin(context, unknown).findings[0].rule_id == "unknown-builtin"


def test_command_cache_mutex_and_attestation_contracts(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "probe.py").write_text("VALUE = 1\n", encoding="utf-8")
    _commit_all(repo, "baseline")
    (repo / "probe.py").write_text("VALUE = 2\n", encoding="utf-8")
    _git(repo, "add", "probe.py")
    policy = load_policy(Path("conductor/candidate_policy.toml"))
    candidate = classify_candidate(resolve_candidate(repo, kind="index"), policy)
    with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
        context = ReviewContext(
            repo=repo,
            snapshot=snapshot,
            candidate=candidate,
            entries=entries,
            policy=policy,
            surface="pre-commit",
            profile="fast",
            owner=None,
            runtime_dir=tmp_path / "runtime",
        )
        prepare_candidate_git_environment(context)
        template = next(check for check in policy.checks if check.kind == "command")
        passing = replace(
            template,
            check_id="command-probe",
            classes=(),
            always=True,
            command=(sys.executable, "-c", "print('command-ok')"),
            version_command=(sys.executable, "--version"),
        )
        version, error = tool_version(context, passing)
        assert version and error is None
        result = run_command_check(context, passing, version=version)
        assert result.status == CheckStatus.PASSED
        assert result.stdout_tail.strip() == "command-ok"
        failing = replace(
            passing,
            command=(sys.executable, "-c", "import sys; sys.exit(5)"),
        )
        assert (
            run_command_check(context, failing, version=version).findings[0].rule_id
            == "analyzer-finding"
        )
        unavailable = run_command_check(
            context, passing, version_error="missing pinned tool"
        )
        assert unavailable.findings[0].rule_id == "required-analyzer-unavailable"
        assert command_cache_material(context, passing, version, ["probe.py"])["files"]

    cache = ResultCache(tmp_path / "cache", ttl_days=1)
    cached_result = CheckResult(
        check_id="cache-probe",
        status=CheckStatus.PASSED,
        duration_ms=3,
        findings=[
            Finding(
                check_id="cache-probe",
                rule_id="evidence",
                severity=Severity.INFO,
                message="cache round trip",
            ).finalize()
        ],
    )
    cache.store("a" * 64, cached_result)
    loaded = cache.load("a" * 64)
    assert (
        loaded and loaded.cache_hit and loaded.findings[0].message == "cache round trip"
    )
    with governance_lock(repo, exclusive=True):
        with pytest.raises(TimeoutError, match="mutex remained busy"):
            with governance_lock(repo, exclusive=True, timeout_seconds=0):
                pass

    receipt = _receipt()
    receipt.surface = "pre-commit"
    receipt.profile = "fast"
    receipt.decision = "pass"
    receipt.candidate["tree_oid"] = candidate.tree_oid
    receipt.policy = {"sha256": policy.digest}
    seal_receipt(receipt)
    write_json_atomic(
        receipt_path(repo, "pre-commit", candidate, "fast"), receipt.to_dict()
    )
    matched, detail = review_engine._matching_precommit_receipt(context)
    assert detail == "ok"
    assert matched and matched["receipt_id"] == receipt.receipt_id
    tampered = receipt.to_dict()
    tampered["receipt_digest"] = "0" * 64
    receipt_file = receipt_path(repo, "pre-commit", candidate, "fast")
    write_json_atomic(receipt_file, tampered)
    assert review_engine._matching_precommit_receipt(context)[0] is None
    write_json_atomic(receipt_file, receipt.to_dict())
    message = tmp_path / "COMMIT_EDITMSG"
    message.write_text("test: candidate\n", encoding="utf-8")
    trailers = append_attestation(message, repo)
    assert trailers["Governance-Tree"] == candidate.tree_oid
    assert append_attestation(message, repo) == trailers
    with pytest.raises(ValueError, match="beginning with 'commit'"):
        run_locked_git_commit(repo, ["status"])


def test_targeted_test_selection_execution_and_coverage(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "probe.py").write_text("def value():\n    return 1\n", encoding="utf-8")
    _commit_all(repo, "baseline")
    (repo / "probe.py").write_text("def value():\n    return 2\n", encoding="utf-8")
    tests = repo / "conductor" / "test_probe.py"
    tests.parent.mkdir()
    (tests.parent / "__init__.py").write_text("", encoding="utf-8")
    tests.write_text(
        "from probe import value\n\ndef test_value_boundary_property():\n    assert value() == 2\n",
        encoding="utf-8",
    )
    _git(repo, "add", "probe.py", "conductor/__init__.py", "conductor/test_probe.py")
    policy = load_policy(Path("conductor/candidate_policy.toml"))
    candidate = classify_candidate(resolve_candidate(repo, kind="index"), policy)
    with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
        context = ReviewContext(
            repo=repo,
            snapshot=snapshot,
            candidate=candidate,
            entries=entries,
            policy=policy,
            surface="pre-commit",
            profile="full",
            owner=None,
            runtime_dir=tmp_path / "runtime",
        )
        evidence, selection = check_test_evidence(context)
        assert selection.tests == ("conductor/test_probe.py",)
        assert {finding.rule_id for finding in evidence.findings} == {
            "graph-evidence-incomplete"
        }
        fast = next(
            check for check in policy.checks if check.check_id == "targeted-tests"
        )
        assert (
            run_targeted_tests(context, selection, fast, coverage=False).status
            == CheckStatus.PASSED
        )
        full = next(
            check for check in policy.checks if check.check_id == "targeted-tests-full"
        )
        covered = run_targeted_tests(context, selection, full, coverage=True)
        assert covered.status == CheckStatus.PASSED, [
            (finding.rule_id, finding.message, finding.evidence)
            for finding in covered.findings
        ]
        assert covered.metrics["changed_coverage_percent"] == 100.0
        empty = ReviewTestSelection((), {}, ())
        assert (
            run_targeted_tests(context, empty, fast, coverage=False).status
            == CheckStatus.SKIPPED
        )

        original_run = review_verification._run_process
        monkeypatch.setattr(
            review_verification,
            "_run_process",
            lambda *_args, **_kwargs: subprocess.CompletedProcess(
                [], 1, "test failed", ""
            ),
        )
        failed = run_targeted_tests(context, selection, fast, coverage=False)
        assert failed.findings[0].rule_id == "targeted-test-failure"

        def crash(*_args: object, **_kwargs: object) -> None:
            raise OSError("deliberate test runner crash")

        monkeypatch.setattr(review_verification, "_run_process", crash)
        crashed = run_targeted_tests(context, selection, fast, coverage=False)
        assert crashed.findings[0].rule_id == "targeted-test-crash"
        monkeypatch.setattr(review_verification, "_run_process", original_run)
        with pytest.raises(ValueError, match="no files object"):
            review_verification._coverage_counts(context, {}, {})


def test_engine_and_tree_integrity_fail_closed_without_candidate_evidence(
    tmp_path: Path,
) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "probe.py").write_text("VALUE = 1\n", encoding="utf-8")
    _commit_all(repo, "baseline")
    (repo / "probe.py").write_text("VALUE = 2\n", encoding="utf-8")
    _git(repo, "add", "probe.py")
    policy = load_policy(Path("conductor/candidate_policy.toml"))
    candidate = classify_candidate(resolve_candidate(repo, kind="index"), policy)
    with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
        context = ReviewContext(
            repo=repo,
            snapshot=snapshot,
            candidate=candidate,
            entries=entries,
            policy=policy,
            surface="post-commit",
            profile="fast",
            owner=None,
            runtime_dir=tmp_path / "runtime",
        )
        engine, integrity = review_engine._engine_integrity(context)
        assert engine["candidate_source_sha256"] == ""
        assert integrity.findings[0].rule_id == "engine-absent-from-candidate"
        bypass, attestation = review_engine._bypass_evidence(context)
        assert bypass["precommit_receipt"] == "missing_or_invalid"
        assert attestation.findings[0].rule_id == "precommit-bypass-recovered"
        assert review_engine._crash_result("probe", RuntimeError("boom")).status == (
            CheckStatus.ERROR
        )

        bad_entries = (
            TreeEntry("Case.py", "100644", "blob", "1" * 40, 1),
            TreeEntry("case.py", "100600", "blob", "2" * 40, 1),
        )
        findings, entry_map = review_checks._tree_integrity_findings(
            replace(context, entries=bad_entries)
        )
        assert {finding.rule_id for finding in findings} == {
            "case-collision",
            "unsupported-git-mode",
        }
        assert set(entry_map) == {"Case.py", "case.py"}
    assert verify_receipt_payload({}) == (False, "receipt_digest is absent")


def test_graph_selected_tests_use_immutable_matching_metadata(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "probe.py").write_text("def value():\n    return 1\n", encoding="utf-8")
    base = _commit_all(repo, "baseline")
    (repo / "probe.py").write_text("def value():\n    return 2\n", encoding="utf-8")
    test_path = repo / "conductor" / "test_graph_probe.py"
    test_path.parent.mkdir()
    test_path.write_text(
        "def test_value_property():\n    assert True\n", encoding="utf-8"
    )
    _git(repo, "add", "probe.py", "conductor/test_graph_probe.py")
    database = repo / ".code-review-graph" / "graph.db"
    database.parent.mkdir()
    connection = sqlite3.connect(database)
    connection.executescript(
        """
        CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT);
        CREATE TABLE nodes (qualified_name TEXT PRIMARY KEY, file_path TEXT, is_test INTEGER);
        CREATE TABLE edges (source_qualified TEXT, target_qualified TEXT, kind TEXT);
        """
    )
    connection.executemany(
        "INSERT INTO metadata VALUES (?, ?)",
        [("git_head_sha", base), ("schema_version", "test"), ("last_updated", "now")],
    )
    connection.executemany(
        "INSERT INTO nodes VALUES (?, ?, ?)",
        [
            ("probe:value", str((repo / "probe.py").resolve()), 0),
            ("test:value", str(test_path.resolve()), 1),
        ],
    )
    connection.execute(
        "INSERT INTO edges VALUES (?, ?, ?)", ("test:value", "probe:value", "calls")
    )
    connection.commit()
    connection.close()
    policy = load_policy(Path("conductor/candidate_policy.toml"))
    candidate = classify_candidate(resolve_candidate(repo, kind="index"), policy)
    with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
        context = ReviewContext(
            repo=repo,
            snapshot=snapshot,
            candidate=candidate,
            entries=entries,
            policy=policy,
            surface="pre-commit",
            profile="fast",
            owner=None,
            runtime_dir=tmp_path / "runtime",
        )
        result, selection = check_test_evidence(context)
    assert result.findings == []
    assert selection.tests == ("conductor/test_graph_probe.py",)
    assert selection.graph["selected_edges"] == 1
    assert selection.graph["head_sha"] == base


def test_index_preserves_deletion_and_rename_identity(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "deleted.txt").write_text("remove me\n", encoding="utf-8")
    (repo / "old-name.txt").write_text("rename me exactly\n", encoding="utf-8")
    _commit_all(repo, "baseline")

    _git(repo, "rm", "--quiet", "deleted.txt")
    _git(repo, "mv", "old-name.txt", "new-name.txt")
    candidate = resolve_candidate(repo, kind="index")
    by_path = {change.path: change for change in candidate.changes}

    deleted = by_path["deleted.txt"]
    assert deleted.status == "D"
    assert deleted.deleted
    assert deleted.new_mode == "000000"
    assert deleted.new_oid == "0" * 40

    renamed = by_path["new-name.txt"]
    assert renamed.status == "R100"
    assert renamed.old_path == "old-name.txt"
    assert renamed.old_oid == renamed.new_oid


def test_rename_retains_old_path_risk_and_policy_classes(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    sensitive = repo / "sensitive"
    sensitive.mkdir()
    (sensitive / "mechanism.py").write_text("VALUE = 1\n", encoding="utf-8")
    _commit_all(repo, "baseline")
    _git(repo, "mv", "sensitive/mechanism.py", "moved.py")
    candidate = resolve_candidate(repo, kind="index")
    policy_text = _minimal_policy_text(
        baseline_expires=(datetime.now(timezone.utc) + timedelta(days=30))
        .date()
        .isoformat()
    ).replace(
        "[classes]\n\n[risk]\nhigh = []",
        '[classes]\nnovel = ["sensitive/**"]\n\n[risk]\nhigh = ["sensitive/**"]',
    )
    policy_path = repo / "policy.toml"
    policy_path.write_text(policy_text, encoding="utf-8")
    policy = load_policy(policy_path)

    classified = classify_candidate(candidate, policy)

    assert classified.changes[0].path == "moved.py"
    assert classified.changes[0].old_path == "sensitive/mechanism.py"
    assert classified.changes[0].risk == "high"
    assert {"python", "novel"}.issubset(classified.changes[0].classes)


def test_materialize_tree_allows_internal_symlink_and_rejects_escape(
    tmp_path: Path,
) -> None:
    repo = _init_repo(tmp_path / "repo")
    docs = repo / "docs"
    docs.mkdir()
    (docs / "target.txt").write_text("inside\n", encoding="utf-8")
    os.symlink("target.txt", docs / "link.txt")
    safe_commit = _commit_all(repo, "safe link")

    safe = resolve_candidate(repo, kind="commit", target_ref=safe_commit)
    with materialize_tree(repo, safe.tree_oid) as (snapshot, entries):
        link = snapshot / "docs" / "link.txt"
        assert link.is_symlink()
        assert os.readlink(link) == "target.txt"
        assert link.read_text(encoding="utf-8") == "inside\n"
        assert any(entry.path == "docs/link.txt" for entry in entries)

    os.symlink("../outside.txt", repo / "escape")
    escape_commit = _commit_all(repo, "escaping link")
    unsafe = resolve_candidate(repo, kind="commit", target_ref=escape_commit)
    with pytest.raises(GitSourceError, match="symlink escapes candidate snapshot"):
        with materialize_tree(repo, unsafe.tree_oid):
            pass


def test_materialize_tree_rejects_blob_before_loading_over_budget(
    tmp_path: Path,
) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "payload.bin").write_bytes(b"0123456789")
    commit = _commit_all(repo, "payload")
    candidate = resolve_candidate(repo, kind="commit", target_ref=commit)

    with pytest.raises(GitSourceError, match="blob exceeds materialization limit"):
        with materialize_tree(repo, candidate.tree_oid, max_blob_bytes=9):
            pass


def test_gitlink_is_represented_without_materializing_foreign_tree(
    tmp_path: Path,
) -> None:
    module = _init_repo(tmp_path / "module")
    (module / "module.txt").write_text("module\n", encoding="utf-8")
    module_commit = _commit_all(module, "module")

    repo = _init_repo(tmp_path / "repo")
    _git(repo, "commit", "--quiet", "--allow-empty", "--message", "baseline")
    _git(
        repo,
        "update-index",
        "--add",
        "--cacheinfo",
        "160000",
        module_commit,
        "vendor/module",
    )
    _git(repo, "commit", "--quiet", "--message", "record submodule")
    candidate = resolve_candidate(repo, kind="commit")

    entry = next(item for item in list_tree(repo, candidate.tree_oid))
    assert (entry.path, entry.mode, entry.object_type, entry.oid) == (
        "vendor/module",
        "160000",
        "commit",
        module_commit,
    )
    assert candidate.changes[0].new_mode == "160000"
    assert candidate.changes[0].new_oid == module_commit
    with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
        assert entries == (entry,)
        assert not (snapshot / "vendor" / "module").exists()


def test_malformed_and_ambiguous_refs_fail_closed(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "value.txt").write_text("one\n", encoding="utf-8")
    _commit_all(repo, "one")
    _git(repo, "branch", "collision")
    (repo / "value.txt").write_text("two\n", encoding="utf-8")
    _commit_all(repo, "two")
    _git(repo, "tag", "collision")

    with pytest.raises(GitSourceError, match="invalid or ambiguous Git ref"):
        resolve_commit(repo, "--verify")
    with pytest.raises(GitSourceError, match="ambiguous"):
        resolve_commit(repo, "collision")


def test_malformed_policy_fails_closed(tmp_path: Path) -> None:
    policy_path = tmp_path / "candidate_policy.toml"
    policy_path.write_text("schema_version = [\n", encoding="utf-8")

    with pytest.raises(PolicyError, match="malformed candidate policy"):
        load_policy(policy_path)


def test_expired_policy_fails_closed(tmp_path: Path) -> None:
    yesterday = datetime.now(timezone.utc).date() - timedelta(days=1)
    policy_path = tmp_path / "candidate_policy.toml"
    policy_path.write_text(
        _minimal_policy_text(baseline_expires=yesterday.isoformat()),
        encoding="utf-8",
    )

    with pytest.raises(PolicyError, match="baseline window expired"):
        load_policy(policy_path)


def test_blanket_exception_scope_fails_closed(tmp_path: Path) -> None:
    today = datetime.now(timezone.utc).date()
    policy_path = tmp_path / "candidate_policy.toml"
    exception = f"""\
[[exceptions]]
id = "too-broad"
check = "candidate-integrity"
path = "**"
owner = "governance"
justification = "This intentionally broad test exception must be rejected."
expires = {(today + timedelta(days=30)).isoformat()}
"""
    policy_path.write_text(
        _minimal_policy_text(
            baseline_expires=(today + timedelta(days=30)).isoformat(),
            exceptions=exception,
        ),
        encoding="utf-8",
    )

    with pytest.raises(PolicyError, match="forbidden blanket scope"):
        load_policy(policy_path)


def test_receipt_digest_is_invalidated_by_candidate_tree_mutation() -> None:
    receipt = _receipt()
    payload = receipt.to_dict()

    assert verify_receipt_payload(payload) == (True, "ok")
    tampered = copy.deepcopy(payload)
    tampered["candidate"]["tree_oid"] = "9" * 40
    valid, detail = verify_receipt_payload(tampered)
    assert not valid
    assert "receipt digest mismatch" in detail


def test_human_sarif_and_junit_reports_preserve_identity_and_findings(
    tmp_path: Path,
) -> None:
    receipt = _receipt()

    summary = human_summary(receipt)
    assert receipt.receipt_id in summary
    assert "candidate-integrity/unsafe-symlink" in summary

    sarif = sarif_payload(receipt)
    runs = cast(list[dict[str, Any]], sarif["runs"])
    run = runs[0]
    assert sarif["version"] == "2.1.0"
    assert run["automationDetails"]["id"] == receipt.receipt_id
    assert run["results"][0]["partialFingerprints"] == {
        "governanceFingerprint": "test-fingerprint-not-a-secret"
    }
    assert run["results"][0]["locations"][0]["physicalLocation"] == {
        "artifactLocation": {"uri": "escape"},
        "region": {"startLine": 2, "startColumn": 1},
    }

    suite = ET.fromstring(junit_xml(receipt))
    assert suite.attrib == {
        "name": "candidate-review:manual:fast",
        "tests": "1",
        "failures": "1",
        "skipped": "0",
        "time": "0.125",
        "id": receipt.receipt_id,
    }
    failure = suite.find("./testcase/failure")
    assert failure is not None
    assert "critical unsafe-symlink" in (failure.text or "")
    json_path = tmp_path / "receipt.json"
    sarif_path = tmp_path / "receipt.sarif"
    junit_path = tmp_path / "receipt.junit.xml"
    write_outputs(
        receipt,
        json_out=json_path,
        sarif_out=sarif_path,
        junit_out=junit_path,
    )
    assert json.loads(json_path.read_text(encoding="utf-8"))["receipt_id"] == (
        receipt.receipt_id
    )
    assert json.loads(sarif_path.read_text(encoding="utf-8"))["version"] == "2.1.0"
    assert ET.parse(junit_path).getroot().attrib["id"] == receipt.receipt_id


def _change(path: str, *, classes: tuple[str, ...] = ()) -> Change:
    return Change(
        status="A",
        path=path,
        old_path=None,
        old_mode="000000",
        new_mode="100644",
        old_oid="0" * 40,
        new_oid="1" * 40,
        classes=classes,
    )


def test_javascript_spec_and_native_tests_are_classified_as_tests() -> None:
    policy = load_policy(Path("conductor/candidate_policy.toml"))
    spec = policy.classify_change(_change("aria_designer/e2e/designer.spec.js"))
    native = policy.classify_change(_change("research/runtime/native/test_kernel.c"))
    production = policy.classify_change(_change("research/tools/mixer_fingerprint.py"))
    assert "test" in spec.classes
    assert "test" in native.classes
    assert "test" not in production.classes


def test_mutation_evidence_skips_when_no_tests_changed(tmp_path: Path) -> None:
    policy = load_policy(Path("conductor/candidate_policy.toml"))
    context = ReviewContext(
        repo=tmp_path,
        snapshot=tmp_path,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid=None,
            target_ref="HEAD",
            changes=(
                _change(
                    "research/tools/mixer_fingerprint.py", classes=("python", "source")
                ),
            ),
        ),
        entries=(),
        policy=policy,
        surface="manual",
        profile="fast",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )
    result = check_mutation_evidence(context)
    assert result.status == CheckStatus.SKIPPED


def test_mutation_evidence_fails_closed_without_registry(tmp_path: Path) -> None:
    policy = load_policy(Path("conductor/candidate_policy.toml"))
    context = ReviewContext(
        repo=tmp_path,
        snapshot=tmp_path,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid=None,
            target_ref="HEAD",
            changes=(
                _change(
                    "research/tests/test_unregistered.py",
                    classes=("python", "source", "test"),
                ),
            ),
        ),
        entries=(),
        policy=policy,
        surface="manual",
        profile="fast",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )
    result = check_mutation_evidence(context)
    assert result.status == CheckStatus.FAILED
    assert {finding.rule_id for finding in result.findings} == {
        "mutation-registry-missing"
    }


def test_mutation_evidence_fails_closed_without_receipt(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    snapshot = tmp_path / "snapshot"
    registry = snapshot / "conductor/mutation_campaigns/registry.json"
    registry.parent.mkdir(parents=True)
    registry.write_text("{}", encoding="utf-8")
    test_path = snapshot / "research/tests/test_unregistered.py"
    test_path.parent.mkdir(parents=True, exist_ok=True)
    test_path.write_text(
        "def test_new_contract():\n    assert True\n", encoding="utf-8"
    )
    monkeypatch.setattr(
        "conductor.mutation_testing.verify_evidence",
        lambda *_args, **_kwargs: {
            "status": "FAIL",
            "checked_test_paths": ["research/tests/test_unregistered.py"],
            "evidence": [],
            "missing_evidence": [
                {
                    "path": "research/tests/test_unregistered.py",
                    "reason": "no registered campaign ranks this test file",
                    "receipt_rejections": [],
                }
            ],
            "malformed_receipts": [],
        },
    )
    policy = load_policy(Path("conductor/candidate_policy.toml"))
    context = ReviewContext(
        repo=tmp_path,
        snapshot=snapshot,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid=None,
            target_ref="HEAD",
            changes=(
                _change(
                    "research/tests/test_unregistered.py",
                    classes=("python", "source", "test"),
                ),
            ),
        ),
        entries=(),
        policy=policy,
        surface="manual",
        profile="fast",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )
    result = check_mutation_evidence(context)
    assert result.status == CheckStatus.FAILED
    assert result.findings[0].rule_id == "missing-mutation-receipt"
    assert result.findings[0].path == "research/tests/test_unregistered.py"


def _new_test_value_context(tmp_path: Path) -> tuple[ReviewContext, Path]:
    snapshot = tmp_path / "snapshot"
    registry = snapshot / "conductor/mutation_campaigns/registry.json"
    registry.parent.mkdir(parents=True)
    registry.write_text("{}", encoding="utf-8")
    test_path = snapshot / "research/tests/test_unregistered.py"
    test_path.parent.mkdir(parents=True, exist_ok=True)
    test_path.write_text(
        "def test_new_contract():\n    assert True\n", encoding="utf-8"
    )
    policy = load_policy(Path("conductor/candidate_policy.toml"))
    context = ReviewContext(
        repo=tmp_path,
        snapshot=snapshot,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid=None,
            target_ref="HEAD",
            changes=(
                _change(
                    "research/tests/test_unregistered.py",
                    classes=("python", "source", "test"),
                ),
            ),
        ),
        entries=(),
        policy=policy,
        surface="manual",
        profile="fast",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )
    receipt_path = (
        snapshot / "conductor/mutation_campaigns/receipts/new_test_value_receipt.json"
    )
    receipt_path.parent.mkdir(parents=True, exist_ok=True)
    return context, receipt_path


def test_mutation_evidence_reports_unavailable_and_malformed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    context, receipt_path = _new_test_value_context(tmp_path)
    from conductor.mutation_testing import CampaignError

    monkeypatch.setattr(
        "conductor.mutation_testing.verify_evidence",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(
            CampaignError("broken registry")
        ),
    )
    result = check_mutation_evidence(context)
    assert result.status == CheckStatus.FAILED
    assert result.findings[0].rule_id == "mutation-evidence-unavailable"

    monkeypatch.setattr(
        "conductor.mutation_testing.verify_evidence",
        lambda *_args, **_kwargs: {
            "status": "FAIL",
            "checked_test_paths": ["research/tests/test_unregistered.py"],
            "evidence": [],
            "missing_evidence": ["not-a-dict"],
            "malformed_receipts": ["receipt.json: truncated"],
        },
    )
    result = check_mutation_evidence(context)
    assert {finding.rule_id for finding in result.findings} == {
        "malformed-mutation-receipt"
    }

    receipt_path.write_text(
        json.dumps({"status": "PASS", "test_value": None}), encoding="utf-8"
    )
    monkeypatch.setattr(
        "conductor.mutation_testing.verify_evidence",
        lambda *_args, **_kwargs: {
            "status": "PASS",
            "checked_test_paths": ["research/tests/test_unregistered.py"],
            "evidence": [
                {
                    "path": "research/tests/test_unregistered.py",
                    "campaign_id": "new_test_value",
                    "receipt": (
                        "conductor/mutation_campaigns/receipts/"
                        "new_test_value_receipt.json"
                    ),
                    "scope": {},
                }
            ],
            "missing_evidence": [],
            "malformed_receipts": [],
        },
    )
    result = check_mutation_evidence(context)
    assert {finding.rule_id for finding in result.findings} == {
        "new-test-value-not-admitted"
    }

    receipt_path.write_text(
        json.dumps(
            {
                "status": "PASS",
                "test_value": {
                    "schema_version": "llm.mutation-testing.test-value.v1",
                    "status": "PASS",
                    "tests": [
                        {
                            "nodeid": (
                                "research/tests/test_unregistered.py::test_new_contract"
                            ),
                            "classification": "CORE",
                        }
                    ],
                },
            }
        ),
        encoding="utf-8",
    )
    result = check_mutation_evidence(context)
    assert result.findings == []
    assert result.metrics["new_test_nodeids"] == [
        "research/tests/test_unregistered.py::test_new_contract"
    ]


def test_locked_commit_reuses_mutex_in_precommit_hook(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    probe = repo / "probe.txt"
    probe.write_text("baseline\n", encoding="utf-8")
    _commit_all(repo, "baseline")

    source_root = Path(review_engine.__file__).resolve().parents[2]
    hook = repo / ".git" / "hooks" / "pre-commit"
    hook.write_text(
        f"""#!{sys.executable}
import os
import sys
from pathlib import Path
sys.path.insert(0, {str(source_root)!r})
from conductor.candidate_review.engine import governance_lock
inherited_fd = os.environ.pop("LLM_GOVERNANCE_COMMIT_LOCK_FD", None)
if inherited_fd is not None:
    try:
        os.close(int(inherited_fd))
    except OSError:
        pass
with governance_lock(Path.cwd(), exclusive=False, timeout_seconds=0.2):
    pass
""",
        encoding="utf-8",
    )
    hook.chmod(0o755)
    probe.write_text("candidate\n", encoding="utf-8")
    _git(repo, "add", "probe.txt")

    assert run_locked_git_commit(repo, ["commit", "-m", "candidate"]) == 0
    assert _git(repo, "show", "HEAD:probe.txt") == "candidate"


def test_inherited_lock_descriptor_validation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = _init_repo(tmp_path / "repo")
    lock_path = review_engine._governance_lock_path(repo)  # noqa: SLF001
    assert review_engine._is_ancestor_process(os.getppid())  # noqa: SLF001
    assert not review_engine._is_ancestor_process(-1)  # noqa: SLF001
    lock_path.parent.mkdir(parents=True)
    lock_path.touch()

    monkeypatch.delenv(review_engine.INHERITED_LOCK_FD_ENV, raising=False)
    assert review_engine._inherited_lock_fd(lock_path) is None  # noqa: SLF001
    monkeypatch.setenv(review_engine.INHERITED_LOCK_FD_ENV, "not-an-integer")
    assert review_engine._inherited_lock_fd(lock_path) is None  # noqa: SLF001
    monkeypatch.setenv(review_engine.INHERITED_LOCK_FD_ENV, "1")
    assert review_engine._inherited_lock_fd(lock_path) is None  # noqa: SLF001

    unrelated = tmp_path / "unrelated.lock"
    with unrelated.open("w", encoding="utf-8") as handle:
        monkeypatch.setenv(review_engine.INHERITED_LOCK_FD_ENV, str(handle.fileno()))
        assert review_engine._inherited_lock_fd(lock_path) is None  # noqa: SLF001

    with lock_path.open("a+", encoding="utf-8") as handle:
        monkeypatch.setenv(review_engine.INHERITED_LOCK_FD_ENV, str(handle.fileno()))
        assert (
            review_engine._inherited_lock_fd(lock_path)  # noqa: SLF001
            == handle.fileno()
        )

    monkeypatch.setenv(review_engine.INHERITED_LOCK_TOKEN_ENV, "short")
    assert not review_engine._inherited_lock_token_valid(lock_path)  # noqa: SLF001
    token = "a" * 64
    monkeypatch.setenv(review_engine.INHERITED_LOCK_TOKEN_ENV, token)
    lock_path.write_text("not json\n", encoding="utf-8")
    assert not review_engine._inherited_lock_token_valid(lock_path)  # noqa: SLF001
    lock_path.write_text(
        json.dumps({"pid": os.getpid(), "token": "b" * 64}), encoding="utf-8"
    )
    assert not review_engine._inherited_lock_token_valid(lock_path)  # noqa: SLF001

    with review_engine._held_governance_lock(  # noqa: SLF001
        repo, exclusive=True, timeout_seconds=1.0, lease_token=token
    ):
        assert review_engine._inherited_lock_token_valid(lock_path)  # noqa: SLF001


def test_changed_coverage_judges_each_risk_class_on_its_own_lines() -> None:
    """A low-risk shortfall must not be judged at the high-risk bar.

    The old rule picked one threshold for the whole candidate from *whether any
    changed path was high-risk*, so a single conductor/** file raised the bar to
    90% for every changed line. Measured on the W7 slices 2026-08-29: the bar and
    the shortfall came from different paths entirely.
    """
    from conductor.candidate_review.verification import _risk_buckets

    per_file = {
        "conductor/agent_a2a.py": {"covered": 95, "measurable": 100},
        "research/synthesis/wavelet.py": {"covered": 80, "measurable": 100},
    }
    risk_of = {"conductor/agent_a2a.py": "high", "research/synthesis/wavelet.py": "low"}
    buckets = _risk_buckets(per_file, risk_of)
    assert buckets["high"] == {"covered": 95, "measurable": 100}
    assert buckets["other"] == {"covered": 80, "measurable": 100}
    # 95% clears 90; 80% clears 75. Pooled it would be 87.5%, which fails the 90
    # bar the old rule would have applied to both -- the case the change fixes.
    pooled = (95 + 80) * 100.0 / 200
    assert pooled < 90.0

    # An unclassified path is treated as other-risk: never silently promoted to
    # the lenient side, never silently held to the strict one.
    assert _risk_buckets(per_file, {})["other"]["measurable"] == 200
    assert _risk_buckets(per_file, {})["high"]["measurable"] == 0
