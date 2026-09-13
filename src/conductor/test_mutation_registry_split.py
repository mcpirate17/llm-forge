"""Contracts for the registry-array-to-fragments migration.

The shared `campaigns` array was the one file every lane wrote to, so every
pair of concurrent PRs conflicted on it (PRs #13, #16, #20 and #22 each
needed a merge-in for that alone). The migration's failure modes are silent:
a half-split registry, a fragment shadowed by a different manifest, or a
split that reorders what the gate reads would each still look like a pass.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from conductor import mutation_coverage
from conductor.mutation_campaign_model import _load_registry
from conductor.mutation_registry_split import split_registry_array
from conductor.mutation_scope import CampaignError


def _registry(repo: Path, campaigns: list[dict]) -> Path:
    """A loadable registry in the default monorepo layout."""
    path = repo / "conductor/mutation_campaigns/registry.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "enforcement": "changed_tests",
                "test_patterns": list(mutation_coverage.CANONICAL_TEST_PATTERNS),
                "receipt_directories": ["conductor/mutation_campaigns/receipts"],
                "campaigns": campaigns,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    return path


def _tree_snapshot(root: Path) -> dict[str, bytes]:
    return {
        path.relative_to(root).as_posix(): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }


def test_split_moves_every_row_into_one_fragment_per_campaign(
    tmp_path: Path,
) -> None:
    """One file per campaign id, envelope intact, reader order sorted by id."""

    repo = tmp_path / "repo"
    registry = _registry(
        repo,
        [
            {"manifest": "conductor/mutation_campaigns/zz_last.json"},
            {"manifest": "conductor/mutation_campaigns/aa_first.json"},
        ],
    )
    before = [row["manifest"] for row in _load_registry(registry, repo)["campaigns"]]
    assert before == [
        "conductor/mutation_campaigns/zz_last.json",
        "conductor/mutation_campaigns/aa_first.json",
    ]

    written = split_registry_array(repo)
    assert written == [
        "conductor/mutation_campaigns/registry.d/zz_last.json",
        "conductor/mutation_campaigns/registry.d/aa_first.json",
    ]
    fragment = repo / "conductor/mutation_campaigns/registry.d/zz_last.json"
    assert json.loads(fragment.read_text(encoding="utf-8")) == {
        "manifest": "conductor/mutation_campaigns/zz_last.json"
    }
    assert fragment.read_text(encoding="utf-8").endswith("\n")

    after = [row["manifest"] for row in _load_registry(registry, repo)["campaigns"]]
    assert after == [
        "conductor/mutation_campaigns/aa_first.json",
        "conductor/mutation_campaigns/zz_last.json",
    ]
    payload = json.loads(registry.read_text(encoding="utf-8"))
    assert payload["campaigns"] == []
    assert payload["enforcement"] == "changed_tests"
    assert payload["test_patterns"] == list(mutation_coverage.CANONICAL_TEST_PATTERNS)


def test_split_is_idempotent(tmp_path: Path) -> None:
    """A second run over a split registry changes no byte."""

    repo = tmp_path / "repo"
    _registry(repo, [{"manifest": "conductor/mutation_campaigns/a.json"}])
    assert split_registry_array(repo) == [
        "conductor/mutation_campaigns/registry.d/a.json"
    ]
    snapshot = _tree_snapshot(repo)
    assert split_registry_array(repo) == []
    assert _tree_snapshot(repo) == snapshot


def test_split_refuses_before_writing_anything(tmp_path: Path) -> None:
    """A duplicate id, a row without a manifest and a shadowing fragment."""

    repo = tmp_path / "repo"
    _registry(
        repo,
        [
            {"manifest": "conductor/mutation_campaigns/dup.json"},
            {"manifest": "conductor/mutation_campaigns/dup.json"},
        ],
    )
    with pytest.raises(CampaignError, match="share the campaign id 'dup'"):
        split_registry_array(repo)
    assert not (repo / "conductor/mutation_campaigns/registry.d").exists()

    _registry(
        repo,
        [{"manifest": "conductor/mutation_campaigns/ok.json"}, {"note": "no manifest"}],
    )
    with pytest.raises(CampaignError, match="has no manifest string"):
        split_registry_array(repo)
    assert not (repo / "conductor/mutation_campaigns/registry.d").exists()

    _registry(repo, [{"manifest": "conductor/mutation_campaigns/a.json"}])
    fragment = repo / "conductor/mutation_campaigns/registry.d/a.json"
    fragment.parent.mkdir(parents=True)
    fragment.write_text(
        json.dumps({"manifest": "conductor/mutation_campaigns/other.json"}) + "\n",
        encoding="utf-8",
    )
    with pytest.raises(CampaignError, match="already registers a different manifest"):
        split_registry_array(repo)
    assert fragment.read_text(encoding="utf-8").startswith("{")
    payload = json.loads(
        (repo / "conductor/mutation_campaigns/registry.json").read_text(encoding="utf-8")
    )
    assert payload["campaigns"] == [
        {"manifest": "conductor/mutation_campaigns/a.json"}
    ]


def _git(repo: Path, *args: str) -> None:
    proc = subprocess.run(
        ["git", *args],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr


def _register(repo: Path, campaign: str) -> None:
    fragment = repo / "conductor/mutation_campaigns/registry.d" / f"{campaign}.json"
    fragment.parent.mkdir(parents=True, exist_ok=True)
    fragment.write_text(
        json.dumps(
            {"manifest": f"conductor/mutation_campaigns/{campaign}.json"}, indent=2
        )
        + "\n",
        encoding="utf-8",
    )
    (repo / f"conductor/mutation_campaigns/{campaign}.json").write_text(
        "{}\n", encoding="utf-8"
    )


def test_two_branches_each_adding_a_campaign_merge_cleanly(tmp_path: Path) -> None:
    """The array form conflicted on every concurrent registration; fragments do not."""

    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "--quiet", "-b", "main")
    _git(repo, "config", "user.name", "Registry Split Test")
    _git(repo, "config", "user.email", "split@test.invalid")
    _registry(
        repo,
        [
            {"manifest": "conductor/mutation_campaigns/one.json"},
            {"manifest": "conductor/mutation_campaigns/two.json"},
        ],
    )
    split_registry_array(repo)
    _git(repo, "add", "-A")
    _git(repo, "commit", "--quiet", "-m", "migrated registry layout")

    for branch, campaign in (("lane-a", "three"), ("lane-b", "four")):
        _git(repo, "checkout", "--quiet", "-b", branch)
        _register(repo, campaign)
        _git(repo, "add", "-A")
        _git(repo, "commit", "--quiet", "-m", f"register {campaign}")

    _git(repo, "checkout", "--quiet", "main")
    _git(repo, "merge", "--no-edit", "--quiet", "lane-a")
    _git(repo, "merge", "--no-edit", "--quiet", "lane-b")

    registry = repo / "conductor/mutation_campaigns/registry.json"
    loaded = [row["manifest"] for row in _load_registry(registry, repo)["campaigns"]]
    assert loaded == [
        "conductor/mutation_campaigns/four.json",
        "conductor/mutation_campaigns/one.json",
        "conductor/mutation_campaigns/three.json",
        "conductor/mutation_campaigns/two.json",
    ]
