from __future__ import annotations

from contextlib import contextmanager
from dataclasses import replace
import json
from pathlib import Path
from types import SimpleNamespace
from typing import Iterator

import pytest

from conductor import mutation_testing


CAMPAIGN_PATH = (
    mutation_testing.REPO_ROOT
    / "conductor/mutation_campaigns/mutation_framework_self.json"
)


def _campaign_payload() -> dict[str, object]:
    payload = json.loads(CAMPAIGN_PATH.read_text(encoding="utf-8"))
    payload.pop("test_scopes", None)
    return payload


def test_self_campaign_ranks_all_tests_and_materializes_three_mutations() -> None:
    campaign = mutation_testing.load_campaign(CAMPAIGN_PATH)

    assert [test.rank for test in campaign.ranked_tests] == list(range(1, 22))
    assert campaign.expected_mutations == 3
    assert len(campaign.planned_mutations) == 3
    assert [mutation.mutation_id for mutation in campaign.mutations] == [
        "authorization_bypass_first_order",
        "timeout_as_kill_first_order",
        "complete_python_scope_first_order",
    ]


def test_self_campaign_inspection_reports_three_materialized_patches(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    campaign = mutation_testing.load_campaign(CAMPAIGN_PATH)
    monkeypatch.setattr(mutation_testing, "source_drift", lambda *_args: [])
    monkeypatch.setattr(mutation_testing, "blocking_processes", lambda *_args: [])

    result = mutation_testing.inspect_campaign(campaign)

    assert result["status"] == "READY"
    assert result["materialized_mutations"] == 3
    assert result["expected_mutations"] == 3
    assert result["resource_status"] == "IDLE"
    assert all(row["materialized"] for row in result["planned_mutations"])


def test_run_requires_explicit_mutation_authority_before_creating_snapshot(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    campaign = _temporary_campaign(tmp_path)
    monkeypatch.setattr(mutation_testing, "source_drift", lambda *_args: [])
    monkeypatch.setattr(mutation_testing, "blocking_processes", lambda *_args: [])

    def fail_snapshot(_repo: Path):
        pytest.fail("an unauthorized campaign must not create an isolated snapshot")

    monkeypatch.setattr(mutation_testing, "isolated_snapshot", fail_snapshot)

    with pytest.raises(mutation_testing.CampaignError, match="--allow-mutations"):
        mutation_testing.run_campaign(
            campaign, allow_mutations=False, repo_root=tmp_path
        )


def test_rank_gaps_fail_closed(tmp_path: Path) -> None:
    payload = _campaign_payload()
    payload["mutations"] = []
    ranked = payload["ranked_tests"]
    assert isinstance(ranked, list)
    assert isinstance(ranked[1], dict)
    ranked[1]["rank"] = 7
    path = tmp_path / "campaign.json"
    path.write_text(json.dumps(payload), encoding="utf-8")

    with pytest.raises(mutation_testing.CampaignError, match="contiguous ranks"):
        mutation_testing.load_campaign(path, repo_root=tmp_path)


def test_baseline_must_execute_every_ranked_test(tmp_path: Path) -> None:
    payload = _campaign_payload()
    payload["mutations"] = []
    baseline = payload["baseline"]
    ranked = payload["ranked_tests"]
    assert isinstance(baseline, dict)
    assert isinstance(baseline["argv"], list)
    assert isinstance(ranked, list)
    assert isinstance(ranked[-1], dict)
    baseline["argv"].remove("conductor/test_mutation_testing.py")
    baseline["argv"].extend(row["nodeid"] for row in ranked[:-1])
    path = tmp_path / "campaign.json"
    path.write_text(json.dumps(payload), encoding="utf-8")

    with pytest.raises(mutation_testing.CampaignError, match="omits ranked tests"):
        mutation_testing.load_campaign(path, repo_root=tmp_path)


def test_complete_python_scope_rejects_omitted_test_node(tmp_path: Path) -> None:
    test_path = tmp_path / "pkg/test_contract.py"
    test_path.parent.mkdir(parents=True)
    test_path.write_text(
        "def test_first():\n    assert True\n\ndef test_second():\n    assert True\n",
        encoding="utf-8",
    )
    relative = "pkg/test_contract.py"
    ranked = (
        mutation_testing.RankedTest(1, f"{relative}::test_first", "first", "first"),
    )

    with pytest.raises(mutation_testing.CampaignError, match="missing=.*test_second"):
        mutation_testing._load_test_scopes(  # noqa: SLF001
            {
                relative: {
                    "mode": "complete",
                    "inventory": "python_ast",
                    "nodeids": [f"{relative}::test_first"],
                }
            },
            source_sha256={relative: mutation_testing._sha256(test_path)},  # noqa: SLF001
            ranked_tests=ranked,
            repo_root=tmp_path,
        )


def test_blocking_processes_returns_matching_process_evidence() -> None:
    output = """
       11 /usr/bin/python unrelated.py
       42 .venv/bin/python -m research.tools.nm_f6_phase22_20m_active train
    """

    blockers = mutation_testing.blocking_processes(
        ["research.tools.nm_f6_phase22_20m_active train"],
        process_output=output,
    )

    assert blockers == [
        {
            "pid": 42,
            "command": (
                ".venv/bin/python -m research.tools.nm_f6_phase22_20m_active train"
            ),
            "matched": ["research.tools.nm_f6_phase22_20m_active train"],
        }
    ]


def test_patch_parser_rejects_file_creation_or_deletion(tmp_path: Path) -> None:
    patch = tmp_path / "delete.patch"
    patch.write_text(
        "diff --git a/example.py b/example.py\n--- a/example.py\n+++ /dev/null\n",
        encoding="utf-8",
    )

    with pytest.raises(mutation_testing.CampaignError, match="create or delete"):
        mutation_testing._patch_paths(patch)  # noqa: SLF001


def _temporary_campaign(tmp_path: Path) -> mutation_testing.Campaign:
    manifest = tmp_path / "campaign.json"
    manifest.write_text("{}\n", encoding="utf-8")
    source = tmp_path / "source.py"
    first_test = tmp_path / "test_one.py"
    second_test = tmp_path / "test_two.py"
    source.write_text("VALUE = 1\n", encoding="utf-8")
    first_test.write_text("def test_one():\n    assert True\n", encoding="utf-8")
    second_test.write_text("def test_two():\n    assert True\n", encoding="utf-8")
    ranked_tests = (
        mutation_testing.RankedTest(1, "test_one.py::test_one", "one", "one"),
        mutation_testing.RankedTest(2, "test_two.py::test_two", "two", "two"),
    )
    planned = tuple(
        mutation_testing.PlannedMutation(
            f"mutation_{index}",
            "source.py",
            f"contract {index}",
            f"description {index}",
            (ranked_tests[index - 1].nodeid,),
        )
        for index in (1, 2)
    )
    mutations = tuple(
        mutation_testing.Mutation(
            mutation.mutation_id,
            tmp_path / f"mutation_{index}.patch",
            "0" * 64,
            ("source.py",),
            mutation.expected_killers,
        )
        for index, mutation in enumerate(planned, start=1)
    )
    return mutation_testing.Campaign(
        manifest_path=manifest,
        manifest_sha256=mutation_testing._sha256(manifest),  # noqa: SLF001
        campaign_id="temporary_campaign",
        title="Temporary unit-test campaign",
        language="python",
        mutation_engine="reviewed_unified_diff",
        expected_mutations=2,
        source_sha256={
            "source.py": mutation_testing._sha256(source),  # noqa: SLF001
            "test_one.py": mutation_testing._sha256(first_test),  # noqa: SLF001
            "test_two.py": mutation_testing._sha256(second_test),  # noqa: SLF001
        },
        ranked_tests=ranked_tests,
        planned_mutations=planned,
        mutations=mutations,
        test_argv=("python", "-m", "pytest", "test_one.py", "test_two.py"),
        timeout_seconds=10,
        blocked_process_substrings=(),
        poll_seconds=1,
        environment={},
        host_read_dependencies=(),
        test_scopes={
            "test_one.py": mutation_testing.TestFileScope(
                "test_one.py", "complete", "python_ast", (ranked_tests[0].nodeid,)
            ),
            "test_two.py": mutation_testing.TestFileScope(
                "test_two.py", "complete", "python_ast", (ranked_tests[1].nodeid,)
            ),
        },
    )


def _write_registry(tmp_path: Path) -> Path:
    registry = tmp_path / "registry.json"
    registry.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "enforcement": "changed_tests",
                "test_patterns": ["test_*.py"],
                "receipt_directories": ["receipts"],
                "campaigns": [{"manifest": "campaign.json"}],
            }
        ),
        encoding="utf-8",
    )
    return registry


def _write_pass_receipt(tmp_path: Path, campaign: mutation_testing.Campaign) -> None:
    receipts = tmp_path / "receipts"
    receipts.mkdir(exist_ok=True)
    receipt = {
        "schema_version": mutation_testing.RECEIPT_SCHEMA,
        "status": "PASS",
        "campaign_id": campaign.campaign_id,
        "manifest": "campaign.json",
        "manifest_sha256": campaign.manifest_sha256,
        "source_sha256": dict(campaign.source_sha256),
        "test_scopes": mutation_testing._test_scopes_payload(campaign),  # noqa: SLF001
        "complete_campaign": True,
        "selected_mutations": [mutation.mutation_id for mutation in campaign.mutations],
        "mutants": [
            {
                "id": mutation.mutation_id,
                "outcome": "KILLED",
                "patch_sha256": mutation.patch_sha256,
            }
            for mutation in campaign.mutations
        ],
        "mutation_score": 1.0,
    }
    (receipts / "pass.json").write_text(json.dumps(receipt), encoding="utf-8")


def _prepare_fake_run(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    *,
    exits: list[Path] | None = None,
) -> None:
    monkeypatch.setattr(
        mutation_testing,
        "inspect_campaign",
        lambda *_args, **_kwargs: {"status": "READY", "readiness_reasons": []},
    )
    monkeypatch.setattr(mutation_testing, "_wait_for_idle", lambda *_args: [])
    monkeypatch.setattr(
        mutation_testing, "_link_host_dependencies", lambda *_args: None
    )
    monkeypatch.setattr(mutation_testing, "_apply_mutation", lambda *_args: None)

    @contextmanager
    def snapshot(_repo: Path) -> Iterator[SimpleNamespace]:
        try:
            yield SimpleNamespace(worktree=tmp_path)
        finally:
            if exits is not None:
                exits.append(tmp_path)

    monkeypatch.setattr(mutation_testing, "isolated_snapshot", snapshot)


def test_baseline_failure_writes_fail_closed_receipt(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    campaign = _temporary_campaign(tmp_path)
    _prepare_fake_run(monkeypatch, tmp_path)
    monkeypatch.setattr(
        mutation_testing,
        "_run_command",
        lambda *_args, **_kwargs: mutation_testing.CommandResult(
            returncode=1,
            timed_out=False,
            duration_seconds=0.1,
            stdout_tail="baseline failed",
            stderr_tail="",
        ),
    )
    receipt = tmp_path / "baseline-failed.json"

    with pytest.raises(mutation_testing.CampaignError, match="baseline failed"):
        mutation_testing.run_campaign(
            campaign,
            allow_mutations=True,
            receipt_path=receipt,
            repo_root=tmp_path,
        )

    payload = json.loads(receipt.read_text(encoding="utf-8"))
    assert payload["status"] == "BASELINE_FAILED"
    assert payload["mutants"] == []


def test_mutant_timeout_is_error_not_kill(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    campaign = _temporary_campaign(tmp_path)
    _prepare_fake_run(monkeypatch, tmp_path)
    results = iter(
        (
            mutation_testing.CommandResult(0, False, 0.1, "baseline", ""),
            mutation_testing.CommandResult(None, True, 1.0, "", "timeout"),
        )
    )
    monkeypatch.setattr(
        mutation_testing, "_run_command", lambda *_args, **_kwargs: next(results)
    )
    receipt = tmp_path / "timeout.json"

    result = mutation_testing.run_campaign(
        campaign,
        allow_mutations=True,
        mutation_ids=[campaign.mutations[0].mutation_id],
        receipt_path=receipt,
        repo_root=tmp_path,
    )

    assert result["status"] == "ERROR"
    assert result["mutants"][0]["outcome"] == "TIMED_OUT"


def test_single_mutant_rerun_is_marked_partial(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    campaign = _temporary_campaign(tmp_path)
    _prepare_fake_run(monkeypatch, tmp_path)
    results = iter(
        (
            mutation_testing.CommandResult(0, False, 0.1, "baseline", ""),
            mutation_testing.CommandResult(1, False, 0.1, "killed", ""),
        )
    )
    monkeypatch.setattr(
        mutation_testing, "_run_command", lambda *_args, **_kwargs: next(results)
    )
    selected = campaign.mutations[1].mutation_id

    result = mutation_testing.run_campaign(
        campaign,
        allow_mutations=True,
        mutation_ids=[selected],
        receipt_path=tmp_path / "partial.json",
        repo_root=tmp_path,
    )

    assert result["status"] == "PASS"
    assert result["complete_campaign"] is False
    assert result["selected_mutations"] == [selected]
    assert result["mutation_score"] == 1.0


def test_unknown_single_mutant_fails_before_snapshot(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    campaign = _temporary_campaign(tmp_path)
    monkeypatch.setattr(
        mutation_testing,
        "inspect_campaign",
        lambda *_args, **_kwargs: {"status": "READY", "readiness_reasons": []},
    )
    monkeypatch.setattr(
        mutation_testing,
        "isolated_snapshot",
        lambda *_args: pytest.fail("unknown selection must not create a snapshot"),
    )

    with pytest.raises(mutation_testing.CampaignError, match="unknown mutation ids"):
        mutation_testing.run_campaign(
            campaign,
            allow_mutations=True,
            mutation_ids=["does_not_exist"],
            repo_root=tmp_path,
        )


def test_patch_hash_drift_is_rechecked_immediately(tmp_path: Path) -> None:
    patch = tmp_path / "mutant.patch"
    patch.write_text("changed after load\n", encoding="utf-8")
    mutation = mutation_testing.Mutation(
        mutation_id="drifted",
        patch_file=patch,
        patch_sha256="0" * 64,
        allowed_paths=("example.py",),
        expected_killers=("test_example.py::test_contract",),
    )

    with pytest.raises(
        mutation_testing.CampaignError, match="changed after campaign load"
    ):
        mutation_testing._apply_mutation(mutation, tmp_path)  # noqa: SLF001


def test_untracked_mutation_patches_are_linked_into_snapshot(tmp_path: Path) -> None:
    campaign = mutation_testing.load_campaign(
        mutation_testing.REPO_ROOT
        / "conductor/mutation_campaigns/mutation_framework_self.json"
    )
    snapshot = tmp_path / "snapshot"
    snapshot.mkdir()

    mutation_testing._link_mutation_patches(  # noqa: SLF001
        campaign, snapshot, mutation_testing.REPO_ROOT
    )

    for mutation in campaign.mutations:
        relative = mutation.patch_file.relative_to(mutation_testing.REPO_ROOT)
        linked = snapshot / relative
        assert linked.is_file()
        assert not linked.is_symlink()
        assert mutation_testing._sha256(linked) == mutation.patch_sha256  # noqa: SLF001


def test_manifest_rejects_patch_path_escape(tmp_path: Path) -> None:
    payload = _campaign_payload()
    mutations = payload["mutations"]
    assert isinstance(mutations, list) and isinstance(mutations[0], dict)
    mutations[0]["patch_file"] = "../../outside.patch"
    path = tmp_path / "campaign.json"
    path.write_text(json.dumps(payload), encoding="utf-8")

    with pytest.raises(mutation_testing.CampaignError, match="normalized"):
        mutation_testing.load_campaign(path, repo_root=tmp_path)


def test_snapshot_cleanup_and_error_receipt_on_unexpected_exception(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    campaign = _temporary_campaign(tmp_path)
    exits: list[Path] = []
    _prepare_fake_run(monkeypatch, tmp_path, exits=exits)
    monkeypatch.setattr(
        mutation_testing,
        "_run_command",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(RuntimeError("boom")),
    )
    receipt = tmp_path / "crash.json"

    with pytest.raises(mutation_testing.CampaignError, match="campaign crashed"):
        mutation_testing.run_campaign(
            campaign,
            allow_mutations=True,
            receipt_path=receipt,
            repo_root=tmp_path,
        )

    assert exits == [tmp_path]
    assert json.loads(receipt.read_text(encoding="utf-8"))["status"] == "ERROR"


def test_atomic_receipt_leaves_no_temporary_file(tmp_path: Path) -> None:
    target = tmp_path / "receipt.json"

    mutation_testing._atomic_json(target, {"status": "PASS"})  # noqa: SLF001

    assert json.loads(target.read_text(encoding="utf-8")) == {"status": "PASS"}
    assert list(tmp_path.glob(".*.tmp")) == []


def test_mandatory_evidence_accepts_current_complete_receipts(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    campaign = _temporary_campaign(tmp_path)
    registry = _write_registry(tmp_path)
    monkeypatch.setattr(mutation_testing, "load_campaign", lambda *_a, **_k: campaign)
    _write_pass_receipt(tmp_path, campaign)
    result = mutation_testing.verify_evidence(
        registry,
        ["test_one.py"],
        repo_root=tmp_path,
    )

    assert result["status"] == "PASS"
    assert len(result["evidence"]) == 1
    assert result["missing_evidence"] == []


def test_mandatory_evidence_rejects_legacy_file_scope(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    campaign = replace(_temporary_campaign(tmp_path), test_scopes={})
    registry = _write_registry(tmp_path)
    monkeypatch.setattr(mutation_testing, "load_campaign", lambda *_a, **_k: campaign)
    _write_pass_receipt(tmp_path, campaign)
    result = mutation_testing.verify_evidence(
        registry,
        ["test_one.py"],
        repo_root=tmp_path,
    )

    assert result["status"] == "FAIL"
    assert result["missing_evidence"][0]["reason"] == (
        "no current complete PASS receipt"
    )
    assert result["missing_evidence"][0]["receipt_rejections"] == [
        "temporary_campaign: campaign lacks explicit test scope"
    ]


def test_mandatory_evidence_rejects_unregistered_changed_test(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    campaign = _temporary_campaign(tmp_path)
    registry = _write_registry(tmp_path)
    monkeypatch.setattr(mutation_testing, "load_campaign", lambda *_a, **_k: campaign)
    result = mutation_testing.verify_evidence(
        registry,
        ["example/tests/test_unregistered.py"],
        repo_root=tmp_path,
    )

    assert result["status"] == "FAIL"
    assert result["missing_evidence"] == [
        {
            "path": "example/tests/test_unregistered.py",
            "reason": "no registered campaign ranks this test file",
            "receipt_rejections": [],
        }
    ]


@pytest.mark.parametrize(
    ("case", "match"),
    [
        ("mode", "mode must be"),
        ("empty", "may not be empty"),
        ("duplicate", "contains duplicates"),
        ("wrong_file", "another file"),
        ("rank_mismatch", "exactly ranked_tests"),
        ("unbound", "not bound"),
        ("unsupported", "inventory is unsupported"),
        ("wrong_suffix", "requires a .py file"),
        ("syntax", "cannot inventory Python tests"),
        ("no_tests", "scope is empty"),
    ],
)
def test_complete_scope_manifest_validation_fails_closed(
    tmp_path: Path, case: str, match: str
) -> None:
    relative = "pkg/test_contract.py"
    test_path = tmp_path / relative
    test_path.parent.mkdir(parents=True)
    test_path.write_text("def test_contract():\n    assert True\n", encoding="utf-8")
    nodeid = f"{relative}::test_contract"
    scope: dict[str, object] = {
        "mode": "complete",
        "inventory": "python_ast",
        "nodeids": [nodeid],
    }
    ranked = (mutation_testing.RankedTest(1, nodeid, "contract", "rationale"),)
    source_sha256 = {relative: mutation_testing._sha256(test_path)}  # noqa: SLF001
    if case == "mode":
        scope["mode"] = "unknown"
    elif case == "empty":
        scope["nodeids"] = []
    elif case == "duplicate":
        scope["nodeids"] = [nodeid, nodeid]
    elif case == "wrong_file":
        scope["nodeids"] = ["other/test_contract.py::test_contract"]
    elif case == "rank_mismatch":
        ranked = (
            mutation_testing.RankedTest(
                1, f"{relative}::test_other", "contract", "rationale"
            ),
        )
    elif case == "unbound":
        source_sha256 = {}
    elif case == "unsupported":
        scope["inventory"] = "javascript_ast"
    elif case == "wrong_suffix":
        relative = "pkg/test_contract.js"
        test_path = tmp_path / relative
        test_path.write_text("test('contract', () => {});\n", encoding="utf-8")
        nodeid = f"{relative}::test_contract"
        scope["nodeids"] = [nodeid]
        ranked = (mutation_testing.RankedTest(1, nodeid, "contract", "rationale"),)
        source_sha256 = {relative: mutation_testing._sha256(test_path)}  # noqa: SLF001
    elif case == "syntax":
        test_path.write_text("def test_contract(\n", encoding="utf-8")
        source_sha256[relative] = mutation_testing._sha256(test_path)  # noqa: SLF001
    elif case == "no_tests":
        test_path.write_text("VALUE = 1\n", encoding="utf-8")
        source_sha256[relative] = mutation_testing._sha256(test_path)  # noqa: SLF001

    with pytest.raises(mutation_testing.CampaignError, match=match):
        mutation_testing._load_test_scopes(  # noqa: SLF001
            {relative: scope},
            source_sha256=source_sha256,
            ranked_tests=ranked,
            repo_root=tmp_path,
        )
