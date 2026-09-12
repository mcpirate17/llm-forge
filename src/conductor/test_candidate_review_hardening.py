"""Fail-closed hardening tests for the mutation-evidence and grandfather gates."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest

from conductor import project_paths
from conductor.candidate_review import verification as review_verification
from conductor.candidate_review.checks import ReviewContext, check_mutation_evidence
from conductor.candidate_review.model import Candidate, Change, CheckStatus
from conductor.candidate_review.policy import load_policy
from conductor.candidate_review.verification import (
    _GrandfatherError,
    _load_grandfathered_nodeids,
    _mutation_receipt_findings,
    _new_test_value_findings,
    _python_test_labels,
)
from conductor.test_candidate_review import (
    _crafted_grandfather_inventory,
    _git,
    _probe_source,
    _write_grandfather_inventory,
)
from conductor.candidate_review.policy_path import resolve_policy_path

EMPTY_TREE_OID = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
PROBE_PATH = "research/tests/test_probe.py"
DEAD_PATH = "research/tests/test_nm_f6_phase22_chinchilla.py"
# A dead path that is NOT tombstoned: only the evaluation-time file check can prune it.
PRUNED_PATH = "research/tests/test_pruned_probe.py"


def _change(path: str) -> Change:
    return Change(
        status="A",
        path=path,
        old_path=None,
        old_mode="000000",
        new_mode="100644",
        old_oid="0" * 40,
        new_oid="1" * 40,
        classes=("python", "source", "test"),
    )


def _anchor_inventory(
    monkeypatch: pytest.MonkeyPatch,
    repo_root: Path,
    snapshot: Path,
    inventory: dict[str, list[str]],
) -> None:
    """Prove a crafted inventory from a real tmp-repo anchor commit."""

    repo_root.mkdir(parents=True, exist_ok=True)
    _git(repo_root, "init", "--quiet", "--initial-branch=main")
    _git(repo_root, "config", "user.name", "Candidate Review Test")
    _git(repo_root, "config", "user.email", "candidate-review@example.invalid")
    for rel_path, labels in inventory.items():
        target = repo_root / rel_path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(_probe_source(labels), encoding="utf-8")
    _git(repo_root, "add", "--", *inventory)
    _git(repo_root, "commit", "--quiet", "--allow-empty", "--message", "anchor")
    monkeypatch.setattr(
        review_verification,
        "GRANDFATHER_ANCHOR_COMMIT_OID",
        _git(repo_root, "rev-parse", "HEAD"),
    )
    monkeypatch.setattr(
        review_verification,
        "GRANDFATHER_ANCHOR_TREE_OID",
        _git(repo_root, "rev-parse", "HEAD^{tree}"),
    )
    path = _write_grandfather_inventory(
        snapshot, _crafted_grandfather_inventory(inventory)
    )
    monkeypatch.setattr(
        review_verification,
        "GRANDFATHER_INVENTORY_SHA256",
        hashlib.sha256(path.read_bytes()).hexdigest(),
    )


def _gate_context(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    *,
    inventory: dict[str, list[str]],
    base_oid: str = "c" * 40,
    anchor_root: Path | None = None,
) -> ReviewContext:
    """Snapshot whose grandfather inventory is proven by a real tmp anchor repo."""

    snapshot = tmp_path / "snapshot"
    registry = snapshot / "conductor/mutation_campaigns/registry.json"
    registry.parent.mkdir(parents=True, exist_ok=True)
    registry.write_text("{}", encoding="utf-8")
    repo = anchor_root if anchor_root is not None else tmp_path
    _anchor_inventory(monkeypatch, repo, snapshot, inventory)
    probe = snapshot / PROBE_PATH
    probe.parent.mkdir(parents=True, exist_ok=True)
    probe.write_text("def test_probe_new():\n    assert True\n", encoding="utf-8")
    return ReviewContext(
        repo=repo,
        snapshot=snapshot,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid=base_oid,
            commit_oid=None,
            target_ref="HEAD",
            changes=(_change(PROBE_PATH),),
        ),
        entries=(),
        policy=load_policy(resolve_policy_path()),
        surface="manual",
        profile="fast",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )


def test_mutation_evidence_containers_fail_closed() -> None:
    findings = _mutation_receipt_findings(
        {
            "missing_evidence": {"path": PROBE_PATH},
            "malformed_receipts": None,
        },
        waived=set(),
    )
    assert [finding.rule_id for finding in findings] == [
        "malformed-evidence-container",
        "malformed-evidence-container",
    ]
    assert "missing_evidence" in findings[0].message
    assert "cannot be evaluated" in findings[0].message
    assert "malformed_receipts" in findings[1].message

    row_findings = _mutation_receipt_findings(
        {"missing_evidence": ["not-an-object", {"path": "waived/path.py"}]},
        waived={"waived/path.py"},
    )
    assert [finding.rule_id for finding in row_findings] == [
        "malformed-mutation-receipt"
    ]
    assert "not-an-object" in row_findings[0].message


def test_mutation_evidence_rows_fail_closed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    context = _gate_context(
        monkeypatch, tmp_path, inventory={PROBE_PATH: ["test_probe_legacy"]}
    )
    findings = _new_test_value_findings(
        context,
        {
            "evidence": [
                42,
                {"campaign_id": "unidentifiable-row"},
                {"path": PROBE_PATH, "receipt": "first.json"},
                {"path": PROBE_PATH, "receipt": "second.json"},
            ]
        },
        {PROBE_PATH: (f"{PROBE_PATH}::test_probe_new",)},
    )
    assert [finding.rule_id for finding in findings] == [
        "malformed-evidence-row",
        "malformed-evidence-row",
        "duplicate-evidence-row",
        "test-value-receipt-unavailable",
    ]
    assert "evidence row 1" in findings[0].message
    assert "evidence row 2" in findings[1].message
    assert "rows 3 and 4" in findings[2].message
    assert PROBE_PATH in findings[2].message
    assert "must be unique" in findings[2].message


def test_mutation_evidence_metrics_survive_malformed_payload(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    context = _gate_context(
        monkeypatch, tmp_path, inventory={PROBE_PATH: ["test_probe_legacy"]}
    )
    monkeypatch.setattr(
        "conductor.mutation_testing.verify_evidence",
        lambda *_args, **_kwargs: {
            "status": "FAIL",
            "checked_test_paths": [PROBE_PATH],
            "evidence": None,
            "missing_evidence": {"path": "not-a-list"},
            "malformed_receipts": None,
        },
    )
    result = check_mutation_evidence(context)
    assert result.status == CheckStatus.FAILED
    assert result.metrics["covered_tests"] == 0
    assert result.metrics["missing_tests"] == 0
    assert {finding.rule_id for finding in result.findings} == {
        "malformed-evidence-container",
        "test-value-receipt-unavailable",
    }


def test_grandfather_matching_inventory_loads_from_anchor(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    inventory = {
        PROBE_PATH: ["test_probe_legacy", "TestShaped::test_method"],
    }
    context = _gate_context(monkeypatch, tmp_path, inventory=inventory)
    assert _load_grandfathered_nodeids(context) == {
        path: frozenset(labels) for path, labels in inventory.items()
    }


def test_grandfather_anchor_commit_absent_fails_closed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    context = _gate_context(
        monkeypatch, tmp_path, inventory={PROBE_PATH: ["test_probe_legacy"]}
    )
    monkeypatch.setattr(review_verification, "GRANDFATHER_ANCHOR_COMMIT_OID", "b" * 40)
    with pytest.raises(_GrandfatherError, match="cannot be proven from git"):
        _load_grandfathered_nodeids(context)


def test_grandfather_anchor_tree_drift_fails_closed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    context = _gate_context(
        monkeypatch, tmp_path, inventory={PROBE_PATH: ["test_probe_legacy"]}
    )
    monkeypatch.setattr(
        review_verification, "GRANDFATHER_ANCHOR_TREE_OID", EMPTY_TREE_OID
    )
    with pytest.raises(_GrandfatherError, match="tree drifted"):
        _load_grandfathered_nodeids(context)


def test_grandfather_crafted_inventory_fails_closed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    context = _gate_context(
        monkeypatch, tmp_path, inventory={PROBE_PATH: ["test_probe_legacy"]}
    )
    crafted = _crafted_grandfather_inventory(
        {PROBE_PATH: ["test_probe_legacy", "test_extra_not_real"]}
    )
    _write_grandfather_inventory(context.snapshot, crafted)
    monkeypatch.setattr(
        review_verification,
        "GRANDFATHER_INVENTORY_SHA256",
        hashlib.sha256(crafted.encode("utf-8")).hexdigest(),
    )
    with pytest.raises(_GrandfatherError, match="does not match the inventory"):
        _load_grandfathered_nodeids(context)


def test_grandfather_inventory_sha256_is_snapshot_independent() -> None:
    """The bound inventory digest must be the shipped bytes, nothing else."""

    payload = json.loads(
        (
            Path(__file__).parent
            / "candidate_review"
            / "grandfathered_test_nodeids_61343f57.json"
        ).read_text(encoding="utf-8")
    )
    assert len(payload["tests"]) == 1026
    assert sum(len(labels) for labels in payload["tests"].values()) == 9202


def test_grandfather_dead_entries_pruned_from_effective_map(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Dead repo paths drop out at evaluation; the anchored proof still binds."""

    inventory = {
        PROBE_PATH: ["test_probe_legacy"],
        PRUNED_PATH: ["test_pruned_legacy"],
    }
    context = _gate_context(
        monkeypatch,
        tmp_path,
        inventory=inventory,
        anchor_root=tmp_path / "anchor",
    )
    (context.repo / PRUNED_PATH).unlink()
    effective = _load_grandfathered_nodeids(context)
    assert effective == {PROBE_PATH: frozenset({"test_probe_legacy"})}
    monkeypatch.setattr(
        review_verification, "GRANDFATHER_ANCHOR_TREE_OID", EMPTY_TREE_OID
    )
    with pytest.raises(_GrandfatherError, match="tree drifted"):
        _load_grandfathered_nodeids(context)


def test_check_mutation_evidence_passes_anchor_repo(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The gate hands verify_evidence the snapshot root and the host repo."""

    context = _gate_context(
        monkeypatch, tmp_path, inventory={PROBE_PATH: ["test_probe_legacy"]}
    )
    captured: dict[str, Path] = {}

    def _spy(
        registry: Path, paths: list[str], *, repo_root: Path, anchor_repo: Path
    ) -> dict[str, object]:
        captured["repo_root"] = repo_root
        captured["anchor_repo"] = anchor_repo
        return {
            "status": "FAIL",
            "checked_test_paths": [PROBE_PATH],
            "evidence": [],
            "missing_evidence": [{"path": PROBE_PATH, "reason": "no receipt"}],
            "malformed_receipts": [],
        }

    monkeypatch.setattr("conductor.mutation_testing.verify_evidence", _spy)
    result = check_mutation_evidence(context)
    assert captured["repo_root"] == context.snapshot
    assert captured["anchor_repo"] == context.repo
    assert result.status == CheckStatus.FAILED


def test_grandfather_tombstoned_lane_revival_stays_gated(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A recreated dead lane never resurrects its anchored exemptions."""

    assert len(review_verification.GRANDFATHER_DEAD_TEST_PATHS) == 12
    inventory = {
        path: [f"test_dead_{index}"]
        for index, path in enumerate(
            sorted(review_verification.GRANDFATHER_DEAD_TEST_PATHS)
        )
    }
    inventory[PROBE_PATH] = ["test_probe_legacy"]
    context = _gate_context(monkeypatch, tmp_path, inventory=inventory)
    (context.repo / DEAD_PATH).write_text(
        "def test_revived_recreated_def():\n    assert True\n", encoding="utf-8"
    )
    effective = _load_grandfathered_nodeids(context)
    assert effective == {PROBE_PATH: frozenset({"test_probe_legacy"})}


def _value_campaign_manifests() -> tuple[tuple[str, dict[str, object]], ...]:
    """Return (registry path, manifest) for every campaign that ranks tests.

    Category (b): the subject is this repo's own campaign corpus, read from
    ``campaigns/registry.json`` through ``conductor.project_paths`` exactly as
    the value gate reads it. Only manifests that rank tests over declared
    ``test_scopes`` carry a value inventory; generated engine manifests do not.
    """

    root = project_paths.host_root(Path(__file__))
    registry = json.loads(project_paths.registry_path(root).read_text(encoding="utf-8"))
    out: list[tuple[str, dict[str, object]]] = []
    for row in registry["campaigns"]:
        manifest = json.loads((root / row["manifest"]).read_text(encoding="utf-8"))
        if "ranked_tests" in manifest and "test_scopes" in manifest:
            out.append((row["manifest"], manifest))
    return tuple(out)


def _derive_inventory_sets(
    manifests: tuple[dict[str, object], ...],
) -> tuple[set[str], set[str]]:
    """Return (live post-anchor defs, grandfathered nodeids) for scoped files."""

    root = project_paths.host_root(Path(__file__))
    inventory = json.loads(
        (
            Path(__file__).parent.parent
            / review_verification.GRANDFATHER_INVENTORY_RELPATH
        ).read_text(encoding="utf-8")
    )
    grandfathered = {
        f"{rel_path}::{label}"
        for rel_path, labels in inventory["tests"].items()
        for label in labels
    }
    post_anchor: set[str] = set()
    scoped_paths: set[str] = set()
    for manifest in manifests:
        for rel_path in manifest["test_scopes"]:
            assert rel_path not in scoped_paths, f"duplicate campaign scope: {rel_path}"
            scoped_paths.add(rel_path)
            labels = set(
                _python_test_labels(
                    (root / rel_path).read_text(encoding="utf-8"), rel_path
                )
            )
            excluded = frozenset(inventory["tests"].get(rel_path, ()))
            post_anchor |= {f"{rel_path}::{label}" for label in labels - excluded}
    return post_anchor, grandfathered


def test_value_inventory_covers_every_post_anchor_def() -> None:
    """Ranked defs outside the inventory must equal the derived live def set."""

    registered = _value_campaign_manifests()
    if not registered:
        root = project_paths.host_root(Path(__file__))
        pytest.skip(
            f"{project_paths.registry_relative(root)} registers no campaign that "
            "ranks tests over declared test_scopes: the value-evidence corpus this "
            "test was written against (claude_receipt_scope_20260902, "
            "claude_value_gate_scope_20260903, claude_value_waivers_20260902) "
            "belongs to the monorepo and was never carried into llm-forge"
        )
    manifests = tuple(manifest for _, manifest in registered)
    post_anchor, grandfathered = _derive_inventory_sets(manifests)
    ranked = [
        row["nodeid"] for manifest in manifests for row in manifest["ranked_tests"]
    ]
    assert len(ranked) == len(set(ranked)), (
        "ranked nodeid appears in multiple campaigns"
    )
    assert set(ranked) - grandfathered == post_anchor
