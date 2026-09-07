"""CLI, benchmark, policy, and ownership contracts for candidate review."""

from __future__ import annotations

import json
import subprocess
from dataclasses import replace
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

import pytest

from conductor.candidate_review import benchmark as review_benchmark
from conductor.candidate_review import cli as review_cli
from conductor.candidate_review import command_runner as review_command_runner
from conductor.candidate_review import ownership as review_ownership
from conductor.candidate_review import policy as review_policy
from conductor.candidate_review.engine import verify_receipt_payload
from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.command_runner import run_command_check, tool_version
from conductor.candidate_review.git_source import (
    classify_candidate,
    materialize_tree,
    resolve_candidate,
)
from conductor.candidate_review.model import (
    Change,
    Finding,
    Severity,
    write_json_atomic,
)
from conductor.candidate_review import identity
from conductor.candidate_review.ownership import (
    OwnershipError,
    claim_store_path,
    create_claim,
    load_claims,
)
from conductor.candidate_review.policy import PolicyError, load_policy
from conductor.test_candidate_review import (
    _commit_all,
    _git,
    _init_repo,
    _install_candidate_engine,
    _minimal_policy_text,
    _receipt,
)
from conductor.candidate_review.policy_path import resolve_policy_path


def test_command_analyzer_failures_are_blocking(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(review_command_runner.shutil, "which", lambda _name: "prlimit")
    monkeypatch.setattr(
        review_command_runner.resource,
        "getrlimit",
        lambda kind: (
            0,
            1024 if kind == review_command_runner.resource.RLIMIT_AS else 2,
        ),
    )
    limited = review_command_runner._limited_command(
        ["probe"], memory_mb=128, timeout_seconds=10
    )
    assert limited[:3] == ["prlimit", "--as=1024", "--cpu=2"]

    repo = _init_repo(tmp_path / "repo")
    (repo / "probe.py").write_text("VALUE = 1\n", encoding="utf-8")
    _commit_all(repo, "baseline")
    (repo / "probe.py").write_text("VALUE = 2\n", encoding="utf-8")
    _git(repo, "add", "probe.py")
    policy = load_policy(resolve_policy_path())
    candidate = classify_candidate(resolve_candidate(repo, kind="index"), policy)
    with materialize_tree(repo, candidate.tree_oid) as (snapshot, entries):
        context = ReviewContext(
            repo,
            snapshot,
            candidate,
            entries,
            policy,
            "pre-commit",
            "fast",
            None,
            tmp_path / "runtime",
        )
        template = next(check for check in policy.checks if check.kind == "command")
        check = replace(template, classes=(), always=True)
        monkeypatch.setattr(
            review_command_runner,
            "_run_process",
            lambda *_args, **_kwargs: subprocess.CompletedProcess(
                [], 7, "", "version failure"
            ),
        )
        assert tool_version(context, check)[1] == "exit 7: version failure"

        def timeout(*_args: object, **_kwargs: object) -> None:
            raise subprocess.TimeoutExpired(["probe"], 1)

        monkeypatch.setattr(review_command_runner, "_run_process", timeout)
        assert (
            run_command_check(context, check, version="1").findings[0].rule_id
            == "analyzer-timeout"
        )

        def crash(*_args: object, **_kwargs: object) -> None:
            raise OSError("deliberate analyzer crash")

        monkeypatch.setattr(review_command_runner, "_run_process", crash)
        assert (
            run_command_check(context, check, version="1").findings[0].rule_id
            == "analyzer-crash"
        )


def test_cli_review_attestation_claim_and_receipt_workflow(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = _init_repo(tmp_path / "repo")
    _install_candidate_engine(repo)
    policy_path = repo / "conductor" / "candidate_policy.toml"
    policy_path.write_text(
        _minimal_policy_text(
            baseline_expires=(date.today() + timedelta(days=30)).isoformat()
        ),
        encoding="utf-8",
    )
    (repo / "probe.txt").write_text("baseline\n", encoding="utf-8")
    _commit_all(repo, "baseline")
    (repo / "probe.txt").write_text("candidate\n", encoding="utf-8")
    _git(repo, "add", "probe.txt")
    json_out = repo / "review.json"

    assert (
        review_cli.main(
            [
                "review",
                "--repo",
                str(repo),
                "--surface",
                "pre-commit",
                "--candidate",
                "index",
                "--profile",
                "fast",
                "--json-out",
                str(json_out),
            ]
        )
        == 0
    )
    payload = json.loads(json_out.read_text(encoding="utf-8"))
    assert payload["decision"] == "pass"
    assert review_cli.main(["verify-receipt", str(json_out)]) == 0

    message = repo / "COMMIT_EDITMSG"
    message.write_text("test: governed candidate\n", encoding="utf-8")
    assert review_cli.main(["attest-message", str(message), "--repo", str(repo)]) == 0
    assert "Governance-Tree:" in message.read_text(encoding="utf-8")
    assert (
        review_cli.main(
            [
                "claim",
                "--repo",
                str(repo),
                "--justification",
                "exercise the complete CLI ownership protocol",
                "probe.txt",
            ]
        )
        == 0
    )
    claims, _digest = load_claims(repo)
    # No --owner was passed: the claim is filed under the lane that will write it,
    # which is the only name its own write gate will present.
    assert claims[0].owner == identity.resolve_owner(repo)
    assert review_cli.main(["claims", "--repo", str(repo)]) == 0
    assert (
        review_cli.main(
            [
                "release-claim",
                claims[0].claim_id,
                "--owner",
                claims[0].owner,
                "--repo",
                str(repo),
            ]
        )
        == 0
    )
    assert not load_claims(repo)[0]
    assert payload["receipt_id"] in capsys.readouterr().out


def test_cli_commands_fail_closed_and_bind_receipts(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "probe.txt").write_text("baseline\n", encoding="utf-8")
    _commit_all(repo, "baseline")
    (repo / "probe.txt").write_text("candidate\n", encoding="utf-8")
    _git(repo, "add", "probe.txt")
    failed_review = tmp_path / "failed-review.json"
    assert (
        review_cli.main(
            [
                "review",
                "--repo",
                str(repo),
                "--surface",
                "pre-commit",
                "--candidate",
                "index",
                "--profile",
                "fast",
                "--policy",
                "../outside.toml",
                "--json-out",
                str(failed_review),
            ]
        )
        == 2
    )
    failure_payload = json.loads(failed_review.read_text(encoding="utf-8"))
    assert failure_payload["decision"] == "fail"
    assert verify_receipt_payload(failure_payload) == (True, "ok")
    malformed = tmp_path / "malformed.json"
    malformed.write_text("{", encoding="utf-8")
    assert review_cli.main(["verify-receipt", str(malformed)]) == 1

    receipt_path_value = tmp_path / "receipt.json"
    write_json_atomic(receipt_path_value, _receipt().to_dict())
    assert (
        review_cli.main(
            [
                "verify-receipt",
                str(receipt_path_value),
                "--repo",
                str(repo),
                "--ref",
                "HEAD",
            ]
        )
        == 1
    )
    missing_message = tmp_path / "missing-message"
    assert (
        review_cli.main(["attest-message", str(missing_message), "--repo", str(repo)])
        == 1
    )
    assert (
        review_cli.main(
            [
                "release-claim",
                "claim-does-not-exist",
                "--owner",
                "Codex",
                "--repo",
                str(repo),
            ]
        )
        == 1
    )

    store = claim_store_path(repo)
    store.parent.mkdir(parents=True, exist_ok=True)
    store.write_text("not-json", encoding="utf-8")
    assert review_cli.main(["claims", "--repo", str(repo)]) == 1
    args = review_cli._parser().parse_args(["fix", "--repo", str(repo), "probe.txt"])
    monkeypatch.setattr(review_cli, "repository_root", lambda _path: repo)
    monkeypatch.setattr(
        review_cli.subprocess,
        "run",
        lambda *_args, **_kwargs: subprocess.CompletedProcess([], 7),
    )
    assert review_cli.fix_command(args) == 7
    args.paths = []
    assert review_cli.fix_command(args) == 2


def test_latency_benchmark_uses_isolated_real_git_candidates(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    candidate_source = Path.cwd()
    source = _init_repo(tmp_path / "source")
    source_paths = [
        *review_benchmark._source_paths(candidate_source),
        "package-lock.json",
    ]
    for relative in source_paths:
        source_path = candidate_source / relative
        target = source / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(source_path.read_bytes())
    _commit_all(source, "isolated benchmark source")
    fixture = review_benchmark._prepare_fixture(source, tmp_path)
    trees: dict[str, str] = {}
    for scenario in review_benchmark.SCENARIOS:
        index = review_benchmark._scenario_index(fixture, tmp_path, scenario)
        trees[scenario] = review_benchmark._git(
            fixture.repo, ["write-tree"], index=index
        )
    assert len(set(trees.values())) == len(review_benchmark.SCENARIOS) - 1
    assert trees["small-python"] == trees["full-review"]

    index = review_benchmark._scenario_index(fixture, tmp_path, "docs-only")
    cold = review_benchmark._review_once(fixture, tmp_path, "docs-only", index, "cold")
    warm = review_benchmark._review_once(fixture, tmp_path, "docs-only", index, "warm")
    assert cold["decision"] == warm["decision"] == "pass"
    assert cold["tree_oid"] == warm["tree_oid"] == trees["docs-only"]
    assert review_benchmark._finding_counts("invalid") == {}
    assert review_benchmark._finding_summary("invalid") == []
    with pytest.raises(review_benchmark.BenchmarkError, match="unknown"):
        review_benchmark._scenario_index(fixture, tmp_path, "unknown")

    output = tmp_path / "benchmark-main.json"
    payload = {"schema_version": 1, "scenarios": {}}
    monkeypatch.setattr(
        review_benchmark,
        "run_benchmarks",
        lambda _source, _scenarios: payload,
    )
    assert (
        review_benchmark.main(
            ["--repo", str(source), "--output", str(output), "--scenario", "docs-only"]
        )
        == 0
    )
    assert json.loads(output.read_text(encoding="utf-8")) == payload


def test_policy_primitives_and_classification_fail_closed(tmp_path: Path) -> None:
    policy = load_policy(resolve_policy_path())
    assert policy.active_checks("fast")
    invalid_calls = [
        (review_policy._string_tuple, ("bad",), {"field": "value"}),
        (review_policy._positive_int, (False,), {"field": "value"}),
        (review_policy._float_percent, (101,), {"field": "value"}),
        (review_policy._bool_value, (1,), {"field": "value"}),
        (review_policy._date_value, ("not-a-date",), {"field": "value"}),
    ]
    for function, args, kwargs in invalid_calls:
        with pytest.raises(PolicyError):
            function(*args, **kwargs)
    with pytest.raises(PolicyError, match="must not be empty"):
        review_policy._string_tuple([], field="value", allow_empty=False)
    with pytest.raises(PolicyError, match="must be <="):
        review_policy._positive_int(17, field="value", maximum=16)
    for path in ("*", "/absolute/path", "single*"):
        with pytest.raises(PolicyError):
            review_policy._validate_exception_path(path)
    with pytest.raises(PolicyError, match="unknown keys"):
        review_policy._parse_check("bad", {"kind": "builtin", "unknown": 1})
    invalid_checks = [
        None,
        {"kind": "unknown", "profiles": ["fast"]},
        {"kind": "builtin", "profiles": ["unknown"]},
        {"kind": "builtin", "profiles": ["fast"], "classes": ["unknown"]},
        {
            "kind": "builtin",
            "profiles": ["fast"],
            "exclude_classes": ["unknown"],
        },
        {"kind": "command", "profiles": ["fast"]},
        {"kind": "builtin", "profiles": ["fast"], "severity": "unknown"},
    ]
    for raw in invalid_checks:
        with pytest.raises(PolicyError):
            review_policy._parse_check("bad", raw)
    for raw in (
        None,
        {"unknown": True},
        {"id": "missing-required-fields"},
        {
            "id": "short",
            "check": "python-ast",
            "path": "conductor/probe.py",
            "owner": "x",
            "justification": "too short",
            "expires": date.today(),
        },
    ):
        with pytest.raises(PolicyError):
            review_policy._parse_exception(raw)
    with pytest.raises(PolicyError, match="baselines must be a table"):
        review_policy._parse_baselines([])
    with pytest.raises(PolicyError, match="invalid schema"):
        review_policy._parse_baselines({"bad": {"path": "only-one-key"}})

    shell = review_policy._intrinsic_classes(
        Change("A", "script.sh", None, "000000", "100755", "0", "1")
    )
    web = review_policy._intrinsic_classes(
        Change("A", "ui.ts", None, "000000", "100644", "0", "1")
    )
    link = review_policy._intrinsic_classes(
        Change("A", "link", None, "000000", "120000", "0", "1")
    )
    assert {"shell", "source"} <= shell
    assert {"web", "source"} <= web
    assert "symlink" in link
    for path, expected in (
        ("pyproject.toml", {"toml", "python_dependency"}),
        ("Cargo.lock", {"rust_dependency"}),
    ):
        classes = review_policy._intrinsic_classes(
            Change("A", path, None, "000000", "100644", "0", "1")
        )
        assert expected <= classes


def _assert_fingerprint_short_circuits_pathless_exception(
    policy: review_policy.Policy, exception: review_policy.ExceptionPolicy
) -> None:
    pathless = Finding(
        check_id="research-integrity",
        rule_id="incomplete-result-provenance",
        severity=Severity.HIGH,
        message="research decision path lacks exact identity/provenance fields: baseline",
    ).finalize()
    assert pathless.path is None
    fingerprint_exception = replace(
        exception,
        exception_id="pathless-probe",
        check_id="research-integrity",
        rule_id="incomplete-result-provenance",
        path="conductor/candidate_policy.toml",
        fingerprint=pathless.fingerprint,
    )
    review_policy.apply_exceptions(
        replace(policy, exceptions=(fingerprint_exception,)), [pathless]
    )
    assert pathless.exception_id == "pathless-probe"

    unrelated = Finding(
        check_id="research-integrity",
        rule_id="incomplete-result-provenance",
        severity=Severity.HIGH,
        message="a different finding entirely",
    ).finalize()
    unmatched = replace(fingerprint_exception, exception_id="pathless-probe-2")
    review_policy.apply_exceptions(
        replace(policy, exceptions=(unmatched,)), [unrelated]
    )
    assert unrelated.exception_id is None


def test_baselines_and_exceptions_are_exact_and_auditable(tmp_path: Path) -> None:
    utc_today = datetime.now(timezone.utc).date()
    baseline = tmp_path / "baseline.json"
    baseline.write_text("{}\n", encoding="utf-8")
    policy = load_policy(resolve_policy_path())
    baseline_policy = review_policy.BaselinePolicy(
        baseline_id="probe",
        path="baseline.json",
        classes=("python",),
        required_profiles=("full",),
    )
    bounded = replace(policy, baselines=(baseline_policy,))
    receipts = review_policy.baseline_receipts(bounded, tmp_path, "full", {"python"})
    assert receipts[0]["path"] == "baseline.json"
    assert review_policy.baseline_receipts(bounded, tmp_path, "fast", {"python"}) == []
    baseline.unlink()
    with pytest.raises(PolicyError, match="required baseline is absent"):
        review_policy.baseline_receipts(bounded, tmp_path, "full", {"python"})

    finding = Finding(
        check_id="python-ast",
        rule_id="owned-debt",
        severity=Severity.HIGH,
        message="bounded finding",
        path="conductor/probe.py",
    ).finalize()
    exception = review_policy.ExceptionPolicy(
        exception_id="owned",
        check_id="python-ast",
        rule_id="owned-debt",
        path="conductor/probe.py",
        fingerprint=finding.fingerprint,
        owner="governance",
        justification="Specific temporary test-only exception.",
        expires=utc_today + timedelta(days=1),
    )
    review_policy.apply_exceptions(replace(policy, exceptions=(exception,)), [finding])
    assert finding.exception_id == "owned"
    duplicate = replace(exception, exception_id="also-owned")
    finding.exception_id = None
    with pytest.raises(PolicyError, match="multiple exceptions"):
        review_policy.apply_exceptions(
            replace(policy, exceptions=(exception, duplicate)), [finding]
        )
    with pytest.raises(PolicyError, match="identifiers must be unique"):
        review_policy._validate_policy(
            replace(policy, exceptions=(exception, exception))
        )
    with pytest.raises(PolicyError, match="unknown check"):
        review_policy._validate_policy(
            replace(
                policy,
                exceptions=(
                    replace(exception, exception_id="unknown", check_id="unknown"),
                ),
            )
        )
    with pytest.raises(PolicyError, match="expired"):
        review_policy._validate_policy(
            replace(
                policy,
                exceptions=(
                    replace(
                        exception,
                        exception_id="expired",
                        expires=utc_today - timedelta(days=1),
                    ),
                ),
            )
        )
    with pytest.raises(PolicyError, match="more than 90 days"):
        review_policy._validate_policy(
            replace(
                policy,
                exceptions=(
                    replace(
                        exception,
                        exception_id="too-long",
                        expires=utc_today + timedelta(days=91),
                    ),
                ),
            )
        )

    _assert_fingerprint_short_circuits_pathless_exception(policy, exception)


def test_ownership_state_rejects_tampering_and_broad_claims(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    (repo / "base.txt").write_text("base\n", encoding="utf-8")
    _commit_all(repo, "baseline")
    # The last two are how a *list* arrives where one path was expected:
    # `CLAIM_PATHS='a, b'` and `--paths a,b` both reach here as a single string.
    # Accepted, they become a claim over a path no file has, which overlaps
    # nothing and reports success while protecting nothing.
    for path in (
        "research",
        "../escape",
        "/absolute",
        "conductor/**",
        "conductor/gate.py,conductor/kb_retrieve.py",
        "conductor/gate.py conductor/kb_retrieve.py",
    ):
        with pytest.raises(OwnershipError, match="narrow and repository-relative"):
            review_ownership.normalize_claim_path(path)
    with pytest.raises(OwnershipError, match="max time must be"):
        create_claim(
            repo,
            owner="Codex",
            paths=["conductor/probe.py"],
            justification="invalid duration test",
            max_minutes=25 * 60,
        )
    claim = create_claim(
        repo,
        owner="Codex",
        paths=["conductor/probe.py"],
        justification="tamper-evident ownership test",
        max_minutes=60,
    )
    store = claim_store_path(repo)
    payload = json.loads(store.read_text(encoding="utf-8"))
    payload["claims"][0]["owner"] = "Mallory"
    store.write_text(json.dumps(payload), encoding="utf-8")
    with pytest.raises(OwnershipError, match="not bound to its content"):
        load_claims(repo)
    assert claim.claim_id
