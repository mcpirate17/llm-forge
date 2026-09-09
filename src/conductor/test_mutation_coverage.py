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
    tracked = repo / "research/tests/test_tracked.py"
    tracked.parent.mkdir(parents=True)
    tracked.write_text("def test_ok():\n    assert True\n", encoding="utf-8")
    subprocess.run(
        ["git", "add", "research/tests/test_tracked.py"], cwd=repo, check=True
    )
    subprocess.run(["git", "commit", "-qm", "tracked test"], cwd=repo, check=True)
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
    test_file = repo / "research/tests/test_tracked.py"
    test_file.parent.mkdir(parents=True)
    test_file.write_text("def test_ok():\n    assert True\n", encoding="utf-8")
    subprocess.run(
        ["git", "add", "research/tests/test_tracked.py"], cwd=repo, check=True
    )
    subprocess.run(["git", "commit", "-qm", "tracked test"], cwd=repo, check=True)

    def fake_verify(registry_path: Path, paths, *, repo_root: Path):
        assert list(paths) == ["research/tests/test_tracked.py"]
        return {
            "status": "FAIL",
            "evidence": [],
            "missing_evidence": [
                {
                    "path": "research/tests/test_tracked.py",
                    "reason": "no registered campaign ranks this test file",
                    "receipt_rejections": [],
                }
            ],
            "malformed_receipts": [],
        }

    monkeypatch.setattr(mutation_coverage, "verify_evidence", fake_verify)
    result = mutation_coverage.coverage_report(registry, repo_root=repo)
    assert result["status"] == "FAIL"
    assert result["total_test_files"] == 1
    assert result["missing_test_files"] == 1
    assert result["enforcement"] == "repository_inventory"


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


def test_mutation_testing_cli_inspect_verify_and_refuse(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    from conductor import mutation_testing

    campaign = (
        mutation_testing.REPO_ROOT
        / "conductor/mutation_campaigns/claude_bash_quiet.json"
    )
    assert mutation_testing.main(["inspect", str(campaign)]) == 0
    assert (
        mutation_testing.main(
            [
                "verify-evidence",
                "--registry",
                str(
                    mutation_testing.REPO_ROOT
                    / "conductor/mutation_campaigns/registry.json"
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
            mutation_testing.REPO_ROOT
            / "conductor/mutation_campaigns/claude_bash_quiet.json"
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
