from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from conductor import mutation_coverage
from conductor.mutation_testing import CampaignError


def _registry_patterns(registry_path: Path, repo_root: Path) -> tuple[str, ...]:
    """Reach the registry-pattern native call the way production reaches it.

    `discover_test_paths` folds patterns into `mutation_test_inventory_native`
    since the 2026-08-31 Rust port (#136), which left named Python wrappers for
    this and for `git` path listing with no caller; both were deleted 2026-09-06
    and re-expressed here so these fail-closed cases keep their coverage.
    """
    from conductor._native import mutation_registry_patterns_native

    return tuple(
        mutation_coverage._native_or_campaign(
            mutation_registry_patterns_native,
            str(repo_root),
            str(registry_path),
            list(mutation_coverage.CANONICAL_TEST_PATTERNS),
        )
    )


def _git_paths(repo_root: Path, args: list[str]) -> tuple[str, ...]:
    from conductor._native import mutation_git_paths_native

    return tuple(
        mutation_coverage._native_or_campaign(
            mutation_git_paths_native,
            str(repo_root),
            list(args),
        )
    )


def _init_repo(path: Path) -> Path:
    path.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=path, check=True)
    subprocess.run(
        ["git", "config", "user.email", "test@example.invalid"], cwd=path, check=True
    )
    subprocess.run(["git", "config", "user.name", "test"], cwd=path, check=True)
    return path


def _registry(repo: Path) -> Path:
    registry = repo / "conductor/mutation_campaigns/registry.json"
    registry.parent.mkdir(parents=True)
    registry.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "enforcement": "changed_tests",
                "test_patterns": list(mutation_coverage.CANONICAL_TEST_PATTERNS),
                "receipt_directories": ["conductor/mutation_campaigns/receipts"],
                "campaigns": [
                    {"manifest": "conductor/mutation_campaigns/placeholder.json"}
                ],
            }
        ),
        encoding="utf-8",
    )
    return registry


def _commit_test_file(
    repo: Path,
    relative: str,
    content: str,
    message: str,
) -> Path:
    """Write one test file and commit it; returns the file's path."""

    path = repo / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    subprocess.run(["git", "add", relative], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-qm", message], cwd=repo, check=True)
    return path


def test_is_test_path_matches_python_and_javascript_specs(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    native_calls: list[tuple[str, tuple[str, ...]]] = []
    native_match = mutation_coverage.is_mutation_test_path_native

    def tracked_native_match(path: str, patterns: list[str]) -> bool:
        native_calls.append((path, tuple(patterns)))
        return native_match(path, patterns)

    monkeypatch.setattr(
        mutation_coverage,
        "is_mutation_test_path_native",
        tracked_native_match,
    )
    patterns = ("**/test_*.py", "**/*.spec.js")
    assert mutation_coverage.is_test_path("research/tests/test_foo.py", patterns)
    assert mutation_coverage.is_test_path(
        "aria_designer/e2e/designer.spec.js", patterns
    )
    assert not mutation_coverage.is_test_path("research/tools/foo.py", patterns)
    assert len(native_calls) == 3

    extended_patterns = ("**/test_[fb]oo.py",)
    assert mutation_coverage.is_test_path(
        "research/tests/test_foo.py", extended_patterns
    )
    assert not mutation_coverage.is_test_path(
        "research/tests/test_zoo.py", extended_patterns
    )
    assert len(native_calls) == 3


def test_discover_and_changed_paths_include_untracked_tests(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    registry = _registry(repo)
    _commit_test_file(
        repo, "research/tests/test_tracked.py", "def test_ok():\n    assert True\n",
        "tracked test",
    )
    untracked = repo / "research/tests/test_new.py"
    untracked.write_text("def test_new():\n    assert True\n", encoding="utf-8")
    (repo / "research/tools/not_module.py").parent.mkdir(parents=True, exist_ok=True)
    (repo / "research/tools/not_module.py").write_text("x = 1\n", encoding="utf-8")

    discovered = mutation_coverage.discover_test_paths(registry, repo_root=repo)
    assert discovered == (
        "research/tests/test_new.py",
        "research/tests/test_tracked.py",
    )
    changed = mutation_coverage.git_changed_test_paths(registry, repo_root=repo)
    assert changed == ("research/tests/test_new.py",)


def test_rust_test_surface_reads_the_file_not_the_name(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    src = repo / "crate/src"
    src.mkdir(parents=True)
    (src / "declares.rs").write_text(
        "pub fn one() -> u8 { 1 }\n\n"
        "#[cfg(test)]\nmod tests {\n"
        "    #[test]\n    fn it_works() {}\n"
        "    #[tokio::test]\n    async fn it_awaits() {}\n"
        "}\n",
        encoding="utf-8",
    )
    (src / "gates_only.rs").write_text(
        "#[cfg(test)]\nuse std::fmt;\n"
        "#[cfg_attr(test, derive(Debug))]\npub struct Thing;\n"
        "// #[test] fn commented_out() {}\n",
        encoding="utf-8",
    )
    (src / "plain.py").write_text("def test_ok():\n    assert True\n", encoding="utf-8")

    assert mutation_coverage.is_rust_test_surface(
        "crate/src/declares.rs", repo_root=repo
    )
    # Gating attributes and a commented-out test declare nothing, and only Rust
    # sources are asked the question at all.
    assert not mutation_coverage.is_rust_test_surface(
        "crate/src/gates_only.rs", repo_root=repo
    )
    assert not mutation_coverage.is_rust_test_surface(
        "crate/src/plain.py", repo_root=repo
    )
    assert not mutation_coverage.is_rust_test_surface(
        "crate/src/absent.rs", repo_root=repo
    )


def test_inventory_finds_rust_tests_no_glob_matches(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    registry = _registry(repo)
    unit = repo / "tooling/native/demo/src/lib.rs"
    unit.parent.mkdir(parents=True)
    unit.write_text(
        "pub fn one() -> u8 { 1 }\n\n#[cfg(test)]\nmod tests {\n"
        "    #[test]\n    fn one_is_one() { assert_eq!(super::one(), 1); }\n}\n",
        encoding="utf-8",
    )
    (repo / "tooling/native/demo/src/plumbing.rs").write_text(
        "pub fn two() -> u8 { 2 }\n", encoding="utf-8"
    )

    patterns = _registry_patterns(registry, repo)
    # The registry's globs are why this file needs a content predicate: none of
    # them matches a Rust unit test living in the module it tests.
    assert not mutation_coverage.is_test_path(
        "tooling/native/demo/src/lib.rs", patterns
    )
    assert mutation_coverage.discover_test_paths(registry, repo_root=repo) == (
        "tooling/native/demo/src/lib.rs",
    )


def test_coverage_report_uses_verify_evidence(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    repo = _init_repo(tmp_path / "repo")
    registry = _registry(repo)
    _commit_test_file(
        repo, "research/tests/test_tracked.py", "def test_ok():\n    assert True\n",
        "tracked test",
    )

    def fake_verify(registry_path: Path, paths, *, repo_root: Path):
        assert list(paths) == ["research/tests/test_tracked.py"]
        return {
            "status": "FAIL",
            "evidence": [],
            "missing_evidence": [
                {
                    "path": "research/tests/test_tracked.py",
                    "reason": "no registered campaign ranks this test file",
                    "reason_kind": "no_campaign",
                    "campaigns": [],
                    "receipt_rejections": [],
                }
            ],
            "rejection_counts": {"no_campaign": 1},
            "malformed_receipts": [],
        }

    monkeypatch.setattr(mutation_coverage, "verify_evidence", fake_verify)
    result = mutation_coverage.coverage_report(registry, repo_root=repo)
    assert result["status"] == "FAIL"
    assert result["total_test_files"] == 1
    assert result["missing_test_files"] == 1
    assert result["enforcement"] == "repository_inventory"
    assert result["schema_version"] == "llm.mutation-testing.coverage.v2"
    assert result["rejection_counts"] == {"no_campaign": 1}


def test_safe_relative_path_and_registry_errors(tmp_path: Path) -> None:
    with pytest.raises(CampaignError, match="normalized"):
        mutation_coverage._safe_relative_path("../escape.py", "path")
    with pytest.raises(CampaignError, match="normalized"):
        mutation_coverage._safe_relative_path("/abs.py", "path")
    with pytest.raises(CampaignError, match="non-empty"):
        mutation_coverage._safe_relative_path("   ", "path")
    outside = tmp_path / "outside.json"
    outside.write_text("{}", encoding="utf-8")
    with pytest.raises(CampaignError, match="inside the repository"):
        _registry_patterns(outside, Path("/home/tim/Projects/LLM"))
    repo = _init_repo(tmp_path / "repo")
    bad = repo / "conductor/mutation_campaigns/registry.json"
    bad.parent.mkdir(parents=True)
    bad.write_text("not json", encoding="utf-8")
    with pytest.raises(CampaignError, match="cannot load"):
        _registry_patterns(bad, repo)
    bad.write_text("[]\n", encoding="utf-8")
    with pytest.raises(CampaignError, match="JSON object"):
        _registry_patterns(bad, repo)
    bad.write_text('{"test_patterns": []}\n', encoding="utf-8")
    with pytest.raises(CampaignError, match="test_patterns"):
        _registry_patterns(bad, repo)
    bad.write_text('{"test_patterns": ["never-a-test"]}\n', encoding="utf-8")
    with pytest.raises(CampaignError, match="canonical inventory"):
        _registry_patterns(bad, repo)


def test_git_failure_and_cache_skip(tmp_path: Path) -> None:
    not_git = tmp_path / "not-git"
    not_git.mkdir()
    with pytest.raises(CampaignError, match="git"):
        _git_paths(not_git, ["status"])
    assert mutation_coverage._should_skip(Path("research/cache/foo/test_x.py"))
    assert mutation_coverage._should_skip(Path(".venv/lib/test_x.py"))


def _changed_result(
    *, missing: list[dict], rejection_counts: dict[str, int]
) -> dict[str, object]:
    return {
        "schema_version": "llm.mutation-testing.changed-evidence.v2",
        "status": "FAIL" if missing else "PASS",
        "enforcement": "changed_tests",
        "checked_test_paths": [row["path"] for row in missing] or ["covered.py"],
        "evidence": [] if missing else [{"path": "covered.py"}],
        "missing_evidence": missing,
        "rejection_counts": rejection_counts,
        "malformed_receipts": [],
    }


def test_changed_exit_codes_split_debt_from_defects(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    debt = _changed_result(
        missing=[
            {
                "path": "src/conductor/test_new.py",
                "reason": "no registered campaign ranks this test file",
                "reason_kind": "no_campaign",
                "campaigns": [],
                "receipt_rejections": [],
            },
            {
                "path": "src/conductor/test_old.py",
                "reason": "no current complete PASS receipt",
                "reason_kind": "not_pass",
                "campaigns": ["c1"],
                "receipt_rejections": [
                    {"receipt": "r1.json", "kind": "superseded",
                     "detail": "receipt superseded by r2.json"},
                    {"receipt": "r0.json", "kind": "runner_map_mismatch",
                     "detail": "runner component hash map mismatch"},
                ],
            },
        ],
        rejection_counts={"no_campaign": 1, "not_pass": 1, "superseded": 1,
                          "runner_map_mismatch": 1},
    )
    defect = _changed_result(
        missing=[
            {
                "path": "src/conductor/test_new.py",
                "reason": "no current complete PASS receipt",
                "reason_kind": "not_pass",
                "campaigns": ["c1"],
                "receipt_rejections": [
                    {"receipt": "r1.json", "kind": "decode_error",
                     "detail": "detail blob is not valid base64: oops"},
                ],
            }
        ],
        rejection_counts={"not_pass": 1, "decode_error": 1},
    )
    covered = _changed_result(missing=[], rejection_counts={})
    monkeypatch.setattr(mutation_coverage, "verify_changed", lambda *_a, **_k: debt)
    assert mutation_coverage.main(["changed", "--registry", "r.json"]) == 6
    monkeypatch.setattr(mutation_coverage, "verify_changed", lambda *_a, **_k: defect)
    assert mutation_coverage.main(["changed", "--registry", "r.json"]) == 5
    monkeypatch.setattr(mutation_coverage, "verify_changed", lambda *_a, **_k: covered)
    assert mutation_coverage.main(["changed", "--registry", "r.json"]) == 0
    capsys.readouterr()


def test_changed_defect_bites_even_when_the_path_has_evidence(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    # A corrupt sibling receipt counts even though another receipt covers the
    # path: the canary's rule, applied to the changed set too.
    result = _changed_result(missing=[], rejection_counts={"superseded": 2})
    assert mutation_coverage.evidence_exit_code(result) == 0
    result["rejection_counts"] = {"schema_error": 1}
    assert mutation_coverage.evidence_exit_code(result) == 5
    capsys.readouterr()


def test_changed_github_annotations_and_summary(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    summary = tmp_path / "summary.md"
    monkeypatch.setenv("GITHUB_STEP_SUMMARY", str(summary))
    result = _changed_result(
        missing=[
            {
                "path": "src/conductor/test_new.py",
                "reason": "no current complete PASS receipt",
                "reason_kind": "not_pass",
                "campaigns": ["c1", "c2"],
                "receipt_rejections": [
                    {"receipt": "r1.json", "kind": "decode_error",
                     "detail": "detail blob is not valid base64: oops"},
                    {"receipt": "r0.json", "kind": "superseded",
                     "detail": "receipt superseded by r1.json"},
                ],
            }
        ],
        rejection_counts={"not_pass": 1, "decode_error": 1, "superseded": 1},
    )
    monkeypatch.setattr(mutation_coverage, "verify_changed", lambda *_a, **_k: result)
    assert mutation_coverage.main(
        ["changed", "--registry", "r.json", "--github"]
    ) == 5
    out = capsys.readouterr().out
    assert "::warning file=src/conductor/test_new.py::" in out
    assert "::error::src/conductor/test_new.py: r1.json:" in out
    table = summary.read_text(encoding="utf-8")
    assert "| path | campaign | status | kind |" in table
    assert "| `src/conductor/test_new.py` | c1, c2 |" in table
    assert "not_pass (decode_error, superseded)" in table


def test_changed_base_reaches_the_merge_base_inventory(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    registry = _registry(repo)
    _commit_test_file(
        repo, "research/tests/test_base_only.py",
        "def test_base():\n    assert True\n", "base",
    )
    base = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, check=True, capture_output=True, text=True
    ).stdout.strip()
    subprocess.run(["git", "checkout", "-qb", "branch"], cwd=repo, check=True)
    _commit_test_file(
        repo, "research/tests/test_branch_only.py",
        "def test_branch():\n    assert True\n", "branch",
    )

    # The native path end to end on a temp repo: only the branch's file is in
    # the merge-base diff, so only it needs evidence in CI.
    changed = mutation_coverage.git_changed_test_paths(
        registry, repo_root=repo, base=base
    )
    assert changed == ("research/tests/test_branch_only.py",)
    # The local shape still diffs the working tree.
    assert mutation_coverage.git_changed_test_paths(registry, repo_root=repo) == ()
    # A base that does not resolve refuses loudly rather than diffing nothing.
    with pytest.raises(CampaignError, match="no-such-ref"):
        mutation_coverage.git_changed_test_paths(
            registry, repo_root=repo, base="no-such-ref"
        )


def test_canary_exit_paths(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    clean = {
        "schema_version": "llm.mutation-testing.coverage.v2",
        "status": "FAIL",
        "enforcement": "repository_inventory",
        "total_test_files": 9,
        "covered_test_files": 0,
        "missing_test_files": 9,
        "evidence": [],
        "missing_evidence": [
            {
                "path": "src/conductor/test_debt.py",
                "reason": "no registered campaign ranks this test file",
                "reason_kind": "no_campaign",
                "campaigns": [],
                "receipt_rejections": [],
            }
        ],
        "rejection_counts": {"no_campaign": 9, "not_pass": 3, "superseded": 5},
        "malformed_receipts": [],
    }
    monkeypatch.setattr(mutation_coverage, "coverage_report", lambda *_a, **_k: clean)
    assert mutation_coverage.main(["canary", "--registry", "r.json"]) == 0
    out = capsys.readouterr().out
    assert '"canary"' in out
    assert '"offending_kinds": []' in out

    unreadable = {
        **clean,
        "rejection_counts": {"no_campaign": 9, "decode_error": 1},
        "missing_evidence": [
            {
                "path": "src/conductor/test_broken.py",
                "reason": "no current complete PASS receipt",
                "reason_kind": "not_pass",
                "campaigns": ["c1"],
                "receipt_rejections": [
                    {"receipt": "r1.json", "kind": "decode_error",
                     "detail": "detail blob does not decompress: frame error"}
                ],
            }
        ],
        "malformed_receipts": ["receipts/gone.json: Expecting value"],
    }
    monkeypatch.setattr(mutation_coverage, "coverage_report", lambda *_a, **_k: unreadable)
    assert mutation_coverage.main(["canary", "--registry", "r.json"]) == 5
    out = capsys.readouterr().out
    assert '"offending_kinds": [' in out
    assert '"decode_error"' in out
    assert "receipts/gone.json" in out


def test_mutation_testing_cli_inspect_verify_and_refuse(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    from conductor import mutation_testing

    # The old monorepo's `conductor/mutation_campaigns/claude_bash_quiet.json`
    # and `.../registry.json` never carried into this standalone repo (see
    # KB note on the split). `mutation_testing.main` resolves every campaign
    # relative to the fixed `REPO_ROOT`, so these two schema-valid fixtures
    # live under the repo-relative `testdata/` tree rather than `tmp_path`.
    campaign = mutation_testing.REPO_ROOT / "src/conductor/testdata/coverage/claude_bash_quiet.json"
    assert mutation_testing.main(["inspect", str(campaign)]) == 0
    assert (
        mutation_testing.main(
            [
                "verify-evidence",
                "--registry",
                str(
                    mutation_testing.REPO_ROOT
                    / "src/conductor/testdata/coverage/claude_bash_quiet_registry.json"
                ),
                "example/tests/test_unregistered.py",
            ]
        )
        == 5
    )
    missing = tmp_path / "missing.json"
    assert mutation_testing.main(["inspect", str(missing)]) == 4
    out = capsys.readouterr().out
    assert "REFUSED" in out


def test_mutation_testing_load_rejects_malformed_campaigns(tmp_path: Path) -> None:
    from conductor import mutation_testing

    path = tmp_path / "campaign.json"
    path.write_text("[]\n", encoding="utf-8")
    with pytest.raises(mutation_testing.CampaignError, match="JSON object"):
        mutation_testing.load_campaign(path, repo_root=tmp_path)
    payload = {
        "schema_version": 1,
        "campaign_id": "x",
        "title": "x",
        "language": "python",
        "mutation_engine": "reviewed_unified_diff",
        "expected_mutations": 1,
        "expected_ranked_tests": 1,
        "source_sha256": {},
        "ranked_tests": [],
        "planned_mutations": [],
        "mutations": [],
        "baseline": {"argv": ["true"], "timeout_seconds": 1},
    }
    path.write_text(json.dumps(payload), encoding="utf-8")
    with pytest.raises(mutation_testing.CampaignError, match="ranked_tests"):
        mutation_testing.load_campaign(path, repo_root=tmp_path)


def test_inspect_cli_returns_not_ready(tmp_path: Path) -> None:
    from conductor import mutation_testing

    payload = json.loads(
        (
            mutation_testing.REPO_ROOT / "src/conductor/testdata/coverage/claude_bash_quiet.json"
        ).read_text(encoding="utf-8")
    )
    payload["mutations"] = []
    path = tmp_path / "campaign.json"
    path.write_text(json.dumps(payload), encoding="utf-8")
    # Copy referenced patches are not needed because mutations=[]
    rc = mutation_testing.main(["inspect", str(path)])
    assert rc in {3, 4}


def test_run_command_timeout_and_ps_scan() -> None:
    from conductor import mutation_testing

    result = mutation_testing._run_command(
        ["sleep", "1"],
        cwd=mutation_testing.REPO_ROOT,
        timeout_seconds=0.01,
        environment={},
    )
    assert result.timed_out is True
    assert result.returncode is None
    blockers = mutation_testing.blocking_processes(["this-string-will-not-match-xyz"])
    assert blockers == []
    with pytest.raises(mutation_testing.CampaignError, match="non-empty string"):
        mutation_testing._require_string("", "label")
    with pytest.raises(mutation_testing.CampaignError, match="list of non-empty"):
        mutation_testing._require_string_list(["", "x"], "label")
