"""Tests for the complete-scope drift reporter."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor.complete_scope_drift import scan

TEST_FILE = "research/tests/test_thing.py"


def _write_repo(
    root: Path,
    *,
    declared: list[str],
    source_tests: list[str],
    registered: bool = True,
    baseline_unloadable: list[str] | None = None,
    manifest_name: str = "campaign.json",
) -> None:
    campaigns = root / "conductor" / "mutation_campaigns"
    campaigns.mkdir(parents=True)
    test_path = root / TEST_FILE
    test_path.parent.mkdir(parents=True)
    test_path.write_text(
        "\n\n".join(f"def {name}():\n    assert True" for name in source_tests) + "\n",
        encoding="utf-8",
    )
    manifest = campaigns / manifest_name
    manifest.write_text(
        json.dumps(
            {
                "test_scopes": {
                    TEST_FILE: {
                        "mode": "complete",
                        "inventory": "python_ast",
                        "nodeids": [f"{TEST_FILE}::{name}" for name in declared],
                    }
                }
            }
        ),
        encoding="utf-8",
    )
    rel_manifest = f"conductor/mutation_campaigns/{manifest_name}"
    (campaigns / "registry.json").write_text(
        json.dumps({"campaigns": [{"manifest": rel_manifest}] if registered else []}),
        encoding="utf-8",
    )
    (campaigns / "reproducibility_baseline.json").write_text(
        json.dumps({"unloadable_manifests": baseline_unloadable or []}),
        encoding="utf-8",
    )


def test_added_test_is_reported_as_new_drift(tmp_path: Path) -> None:
    _write_repo(
        tmp_path,
        declared=["test_a", "test_b"],
        source_tests=["test_a", "test_b", "test_c"],
    )
    drifts = scan(tmp_path, None)
    assert len(drifts) == 1
    drift = drifts[0]
    assert drift.missing == [f"{TEST_FILE}::test_c"]
    assert drift.extra == []
    assert drift.known is False


def test_removed_test_is_reported_as_extra(tmp_path: Path) -> None:
    _write_repo(
        tmp_path,
        declared=["test_a", "test_b"],
        source_tests=["test_a"],
    )
    (drift,) = scan(tmp_path, None)
    assert drift.extra == [f"{TEST_FILE}::test_b"]
    assert drift.missing == []


def test_reordering_alone_is_drift(tmp_path: Path) -> None:
    """The audit compares node IDs in source order, so a swap breaks it."""
    _write_repo(
        tmp_path,
        declared=["test_b", "test_a"],
        source_tests=["test_a", "test_b"],
    )
    (drift,) = scan(tmp_path, None)
    assert drift.missing == []
    assert drift.extra == []
    assert drift.reordered is True


def test_scope_in_sync_reports_nothing(tmp_path: Path) -> None:
    _write_repo(
        tmp_path,
        declared=["test_a", "test_b"],
        source_tests=["test_a", "test_b"],
    )
    assert scan(tmp_path, None) == []


def test_unregistered_manifest_is_ignored(tmp_path: Path) -> None:
    """Only registry.json drives the audit, so unregistered drift cannot fail CI."""
    _write_repo(
        tmp_path,
        declared=["test_a"],
        source_tests=["test_a", "test_b"],
        registered=False,
    )
    assert scan(tmp_path, None) == []


def test_baseline_absorbed_drift_is_labelled_known(tmp_path: Path) -> None:
    _write_repo(
        tmp_path,
        declared=["test_a"],
        source_tests=["test_a", "test_b"],
        baseline_unloadable=["conductor/mutation_campaigns/campaign.json"],
    )
    (drift,) = scan(tmp_path, None)
    assert drift.known is True


def test_restrict_skips_files_that_were_not_changed(tmp_path: Path) -> None:
    _write_repo(
        tmp_path,
        declared=["test_a"],
        source_tests=["test_a", "test_b"],
    )
    assert scan(tmp_path, frozenset({"some/other/file.py"})) == []
    assert len(scan(tmp_path, frozenset({TEST_FILE}))) == 1


@pytest.mark.parametrize("mode", ["partial", "unset"])
def test_non_complete_scopes_are_ignored(tmp_path: Path, mode: str) -> None:
    campaigns = tmp_path / "conductor" / "mutation_campaigns"
    campaigns.mkdir(parents=True)
    test_path = tmp_path / TEST_FILE
    test_path.parent.mkdir(parents=True)
    test_path.write_text("def test_a():\n    assert True\n", encoding="utf-8")
    scope: dict[str, object] = {
        "inventory": "python_ast",
        "nodeids": [f"{TEST_FILE}::test_missing"],
    }
    if mode != "unset":
        scope["mode"] = mode
    (campaigns / "campaign.json").write_text(
        json.dumps({"test_scopes": {TEST_FILE: scope}}), encoding="utf-8"
    )
    (campaigns / "registry.json").write_text(
        json.dumps(
            {"campaigns": [{"manifest": "conductor/mutation_campaigns/campaign.json"}]}
        ),
        encoding="utf-8",
    )
    (campaigns / "reproducibility_baseline.json").write_text(
        json.dumps({"unloadable_manifests": []}), encoding="utf-8"
    )
    assert scan(tmp_path, None) == []
