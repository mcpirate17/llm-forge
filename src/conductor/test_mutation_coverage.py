from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from conductor import mutation_coverage
from conductor.mutation_testing import CampaignError


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


def test_is_test_path_matches_python_and_javascript_specs() -> None:
    patterns = ("**/test_*.py", "**/*.spec.js")
    assert mutation_coverage.is_test_path("research/tests/test_foo.py", patterns)
    assert mutation_coverage.is_test_path(
        "aria_designer/e2e/designer.spec.js", patterns
    )
    assert not mutation_coverage.is_test_path("research/tools/foo.py", patterns)


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


def test_scaffold_campaign_ranks_tests_and_does_not_generate_mutants(
    tmp_path: Path,
) -> None:
    repo = _init_repo(tmp_path / "repo")
    source = repo / "pkg/mod.py"
    source.parent.mkdir()
    source.write_text("VALUE = 1\n", encoding="utf-8")
    test_file = repo / "pkg/test_mod.py"
    test_file.write_text(
        "def test_value():\n    assert True\n\n"
        "class TestGroup:\n    def test_inner(self):\n        assert True\n",
        encoding="utf-8",
    )
    output = repo / "campaign.json"
    result = mutation_coverage.scaffold_campaign(
        "pkg/test_mod.py",
        sources=["pkg/mod.py"],
        output_path=output,
        repo_root=repo,
    )
    payload = json.loads(output.read_text(encoding="utf-8"))
    assert result["status"] == "NOT_READY"
    assert payload["mutations"] == []
    assert payload["expected_mutations"] == 1
    assert [row["nodeid"] for row in payload["ranked_tests"]] == [
        "pkg/test_mod.py::test_value",
        "pkg/test_mod.py::TestGroup::test_inner",
    ]
    assert payload["source_sha256"]["pkg/mod.py"]
    assert payload["planned_mutations"][0]["target_path"] == "pkg/mod.py"


def test_scaffold_rejects_missing_test(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path / "repo")
    with pytest.raises(CampaignError, match="does not exist"):
        mutation_coverage.scaffold_campaign("missing/test_mod.py", repo_root=repo)


def test_safe_relative_path_and_registry_errors(tmp_path: Path) -> None:
    with pytest.raises(CampaignError, match="normalized"):
        mutation_coverage._safe_relative_path("../escape.py", "path")  # noqa: SLF001
    with pytest.raises(CampaignError, match="normalized"):
        mutation_coverage._safe_relative_path("/abs.py", "path")  # noqa: SLF001
    with pytest.raises(CampaignError, match="non-empty"):
        mutation_coverage._safe_relative_path("   ", "path")  # noqa: SLF001
    outside = tmp_path / "outside.json"
    outside.write_text("{}", encoding="utf-8")
    with pytest.raises(CampaignError, match="inside the repository"):
        mutation_coverage._registry_patterns(outside, Path("/home/tim/Projects/LLM"))
    repo = _init_repo(tmp_path / "repo")
    bad = repo / "conductor/mutation_campaigns/registry.json"
    bad.parent.mkdir(parents=True)
    bad.write_text("not json", encoding="utf-8")
    with pytest.raises(CampaignError, match="cannot load"):
        mutation_coverage._registry_patterns(bad, repo)
    bad.write_text("[]\n", encoding="utf-8")
    with pytest.raises(CampaignError, match="JSON object"):
        mutation_coverage._registry_patterns(bad, repo)
    bad.write_text('{"test_patterns": []}\n', encoding="utf-8")
    with pytest.raises(CampaignError, match="test_patterns"):
        mutation_coverage._registry_patterns(bad, repo)
    bad.write_text('{"test_patterns": ["never-a-test"]}\n', encoding="utf-8")
    with pytest.raises(CampaignError, match="canonical inventory"):
        mutation_coverage._registry_patterns(bad, repo)


def test_git_failure_and_cache_skip(tmp_path: Path) -> None:
    not_git = tmp_path / "not-git"
    not_git.mkdir()
    with pytest.raises(CampaignError, match="git"):
        mutation_coverage._git_paths(not_git, ["status"])  # noqa: SLF001
    assert mutation_coverage._should_skip(  # noqa: SLF001
        Path("research/cache/foo/test_x.py")
    )
    assert mutation_coverage._should_skip(Path(".venv/lib/test_x.py"))  # noqa: SLF001


def test_verify_changed_and_scaffold_parse_errors(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    repo = _init_repo(tmp_path / "repo")
    registry = _registry(repo)
    seed = repo / "README.md"
    seed.write_text("seed\n", encoding="utf-8")
    subprocess.run(["git", "add", "README.md"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-qm", "seed"], cwd=repo, check=True)
    test_file = repo / "research/tests/test_new.py"
    test_file.parent.mkdir(parents=True)
    test_file.write_text("def test_new():\n    assert True\n", encoding="utf-8")

    def fake_verify(registry_path: Path, paths, *, repo_root: Path):
        return {
            "status": "PASS",
            "evidence": [{"path": "research/tests/test_new.py"}],
            "missing_evidence": [],
            "malformed_receipts": [],
        }

    monkeypatch.setattr(mutation_coverage, "verify_evidence", fake_verify)
    result = mutation_coverage.verify_changed(registry, repo_root=repo)
    assert result["status"] == "PASS"
    assert result["enforcement"] == "changed_tests"
    broken = repo / "pkg/test_broken.py"
    broken.parent.mkdir()
    broken.write_text("def test_oops(\n", encoding="utf-8")
    with pytest.raises(CampaignError, match="cannot parse"):
        mutation_coverage.scaffold_campaign("pkg/test_broken.py", repo_root=repo)
    empty = repo / "pkg/test_empty.py"
    empty.write_text("VALUE = 1\n", encoding="utf-8")
    with pytest.raises(CampaignError, match="no test functions"):
        mutation_coverage.scaffold_campaign("pkg/test_empty.py", repo_root=repo)
    with pytest.raises(CampaignError, match="does not exist"):
        mutation_coverage.scaffold_campaign(
            "pkg/test_new.py", sources=["pkg/missing.py"], repo_root=repo
        )


def test_cli_coverage_changed_scaffold_and_refusal(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(
        mutation_coverage,
        "coverage_report",
        lambda registry: {"status": "FAIL", "total_test_files": 1},
    )
    assert (
        mutation_coverage.main(
            ["coverage", "--registry", "conductor/mutation_campaigns/registry.json"]
        )
        == 5
    )
    monkeypatch.setattr(
        mutation_coverage,
        "verify_changed",
        lambda registry: {"status": "PASS", "checked_test_paths": []},
    )
    assert mutation_coverage.main(["changed"]) == 0
    monkeypatch.setattr(
        mutation_coverage,
        "scaffold_campaign",
        lambda *a, **k: {"status": "NOT_READY"},
    )
    assert (
        mutation_coverage.main(
            ["scaffold", "pkg/test_mod.py", "--source", "pkg/mod.py"]
        )
        == 0
    )
    monkeypatch.setattr(
        mutation_coverage,
        "coverage_report",
        lambda registry: (_ for _ in ()).throw(CampaignError("boom")),
    )
    assert mutation_coverage.main(["coverage"]) == 4
    captured = capsys.readouterr()
    assert "REFUSED" in captured.out


def test_mutation_testing_cli_inspect_verify_and_refuse(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    from conductor import mutation_testing

    campaign = (
        mutation_testing.REPO_ROOT
        / "conductor/mutation_campaigns/mutation_framework_self.json"
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


def test_wait_for_idle_and_host_dependencies(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    from conductor import mutation_testing

    campaign = tmp_path / "campaign.json"
    campaign.write_text("{}", encoding="utf-8")
    ranked = (
        mutation_testing.RankedTest(1, "t.py::test_a", "a", "a"),
        mutation_testing.RankedTest(2, "t.py::test_b", "b", "b"),
    )
    planned = (
        mutation_testing.PlannedMutation("m1", "src.py", "c", "d", (ranked[0].nodeid,)),
    )
    obj = mutation_testing.Campaign(
        manifest_path=campaign,
        manifest_sha256="a" * 64,
        campaign_id="cid",
        title="t",
        language="python",
        mutation_engine="reviewed_unified_diff",
        expected_mutations=1,
        source_sha256={"src.py": "b" * 64},
        ranked_tests=ranked,
        planned_mutations=planned,
        mutations=(),
        test_argv=("true",),
        timeout_seconds=1,
        blocked_process_substrings=("busy",),
        poll_seconds=1,
        environment={},
        host_read_dependencies=("dep.txt",),
    )
    monkeypatch.setattr(
        mutation_testing,
        "blocking_processes",
        lambda *_a, **_k: [{"pid": 1, "command": "busy", "matched": ["busy"]}],
    )
    blockers = mutation_testing._wait_for_idle(obj, 0)  # noqa: SLF001
    assert blockers
    with pytest.raises(mutation_testing.CampaignError, match="missing"):
        mutation_testing._link_host_dependencies(obj, tmp_path, tmp_path)  # noqa: SLF001
    host = tmp_path / "host"
    host.mkdir()
    (host / "dep.txt").write_text("x\n", encoding="utf-8")
    snap = tmp_path / "snap"
    snap.mkdir()
    mutation_testing._link_host_dependencies(obj, snap, host)  # noqa: SLF001
    assert (snap / "dep.txt").read_text(encoding="utf-8") == "x\n"
    assert (snap / "dep.txt").is_file()
    assert not (snap / "dep.txt").is_symlink()
    with pytest.raises(mutation_testing.CampaignError, match="already contains"):
        mutation_testing._link_host_dependencies(obj, snap, host)  # noqa: SLF001
    with pytest.raises(mutation_testing.CampaignError, match="at least one"):
        mutation_testing._select_mutations(obj, [])  # noqa: SLF001
    with pytest.raises(mutation_testing.CampaignError, match="duplicate"):
        mutation_testing._select_mutations(obj, ["m1", "m1"])  # noqa: SLF001
    inspected = mutation_testing.inspect_campaign(obj, repo_root=tmp_path)
    assert inspected["status"] == "NOT_READY"
    raw = json.loads(
        (
            mutation_testing.REPO_ROOT
            / "conductor/mutation_campaigns/mutation_framework_self.json"
        ).read_text(encoding="utf-8")
    )
    raw["mutations"] = []
    cases = [
        ({**raw, "ranked_tests": []}, "ranked_tests"),
        (
            {
                **raw,
                "ranked_tests": [
                    {"rank": "x", "nodeid": "n", "contract": "c", "rationale": "r"}
                ],
            },
            "rank",
        ),
        (
            {
                **raw,
                "ranked_tests": [
                    {"rank": 1, "nodeid": "same", "contract": "c", "rationale": "r"},
                    {"rank": 2, "nodeid": "same", "contract": "c", "rationale": "r"},
                ],
            },
            "duplicate nodeids",
        ),
        ({**raw, "planned_mutations": None}, "planned_mutations"),
        (
            {**raw, "expected_mutations": len(raw["planned_mutations"]) + 1},
            "planned mutation",
        ),
    ]
    for body, match in cases:
        path = tmp_path / "case.json"
        path.write_text(json.dumps(body), encoding="utf-8")
        with pytest.raises(mutation_testing.CampaignError, match=match):
            mutation_testing.load_campaign(path, repo_root=tmp_path)


def test_inspect_cli_returns_not_ready(tmp_path: Path) -> None:
    from conductor import mutation_testing

    payload = json.loads(
        (
            mutation_testing.REPO_ROOT
            / "conductor/mutation_campaigns/mutation_framework_self.json"
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

    result = mutation_testing._run_command(  # noqa: SLF001
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
        mutation_testing._require_string("", "label")  # noqa: SLF001
    with pytest.raises(mutation_testing.CampaignError, match="list of non-empty"):
        mutation_testing._require_string_list(["", "x"], "label")  # noqa: SLF001
