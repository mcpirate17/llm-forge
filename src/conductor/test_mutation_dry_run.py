"""Tests for conductor.mutation_dry_run."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

import conductor.mutation_testing as mt
from conductor.mutation_dry_run import dry_run, main
from conductor.test_mutation_testing import _prepare_fake_run, _temporary_campaign


def _stub_execution(monkeypatch: pytest.MonkeyPatch, returncodes: list[int]) -> None:
    """Feed one pytest exit code per run: the baseline first, then each mutant."""
    remaining = iter(returncodes)

    def run_command(*_args: object, **_kwargs: object) -> mt.CommandResult:
        return mt.CommandResult(
            returncode=next(remaining),
            timed_out=False,
            duration_seconds=0.0,
            stdout_tail="",
            stderr_tail="",
        )

    monkeypatch.setattr(mt, "_run_command", run_command)
    monkeypatch.setattr(mt, "_link_mutation_patches", lambda *_args: None)
    monkeypatch.setattr(mt, "source_drift", lambda *_args: [])


def test_dry_run_refuses_without_mutation_authority(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _prepare_fake_run(monkeypatch, tmp_path)
    _stub_execution(monkeypatch, [0, 0, 0])
    with pytest.raises(mt.CampaignError, match="allow-mutations"):
        dry_run(
            _temporary_campaign(tmp_path), allow_mutations=False, repo_root=tmp_path
        )


@pytest.mark.parametrize(
    ("returncodes", "outcomes", "survivors"),
    [
        ([0, 1, 1], ["KILLED", "KILLED"], []),
        ([0, 0, 1], ["SURVIVED", "KILLED"], ["mutation_1"]),
        ([0, 1, 0], ["KILLED", "SURVIVED"], ["mutation_2"]),
    ],
)
def test_dry_run_reports_outcomes_and_writes_nothing(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    returncodes: list[int],
    outcomes: list[str],
    survivors: list[str],
) -> None:
    campaign = _temporary_campaign(tmp_path)
    _prepare_fake_run(monkeypatch, tmp_path)
    _stub_execution(monkeypatch, returncodes)
    before = sorted(path.name for path in tmp_path.iterdir())

    report = dry_run(campaign, allow_mutations=True, repo_root=tmp_path)

    assert [row["outcome"] for row in report["mutants"]] == outcomes
    assert report["survivors"] == survivors
    assert report["receipt_written"] is False and report["dry_run"] is True
    assert sorted(path.name for path in tmp_path.iterdir()) == before


def test_dry_run_fails_closed_when_baseline_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    campaign = _temporary_campaign(tmp_path)
    _prepare_fake_run(monkeypatch, tmp_path)
    _stub_execution(monkeypatch, [1, 1, 1])
    with pytest.raises(mt.CampaignError, match="baseline failed"):
        dry_run(campaign, allow_mutations=True, repo_root=tmp_path)


def test_main_exit_codes_track_survivors_and_authority(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    campaign = _temporary_campaign(tmp_path)
    _prepare_fake_run(monkeypatch, tmp_path)
    monkeypatch.setattr(mt, "load_campaign", lambda *_args, **_kwargs: campaign)
    manifest = str(campaign.manifest_path)

    _stub_execution(monkeypatch, [0, 0, 1])
    code = main([manifest, "--allow-mutations", "--json", "--repo", str(tmp_path)])
    report = json.loads(capsys.readouterr().out)
    assert code == 1
    assert report["survivors"] == ["mutation_1"]

    _stub_execution(monkeypatch, [0, 1, 1])
    code = main([manifest, "--allow-mutations", "--repo", str(tmp_path)])
    out = capsys.readouterr().out
    assert code == 0
    assert "NO RECEIPT WRITTEN" in out and "every mutant killed" in out

    code = main([manifest, "--repo", str(tmp_path)])
    assert code == 2
    assert "allow-mutations" in capsys.readouterr().err
