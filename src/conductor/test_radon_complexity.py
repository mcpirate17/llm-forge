"""The complexity ratchet must bite on growth, not only on new symbols."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor.candidate_review.policy import load_policy
from conductor.radon_complexity import (
    DEFAULT_PATHS,
    REPO_ROOT,
    _load_baseline,
    _run_check,
    _scan,
)


def _finding(key: str, complexity: int, rank: str = "D") -> dict[str, object]:
    path, name = key.split("::", 1)
    return {
        "key": key,
        "path": path,
        "name": name,
        "type": "F",
        "line": 1,
        "complexity": complexity,
        "rank": rank,
    }


def _baseline(tmp_path: Path, entries: list[dict[str, object]]) -> Path:
    path = tmp_path / "baseline.json"
    path.write_text(
        json.dumps({"minimum_rank": "D", "findings": entries}), encoding="utf-8"
    )
    return path


KEY = "conductor/example.py::widget"


def test_conductor_is_scanned_by_default() -> None:
    # The ratchet lives in conductor and did not measure its own home.
    assert "conductor" in DEFAULT_PATHS
    findings, parse_errors = _scan(["conductor/radon_complexity.py"], [])
    assert parse_errors == []
    assert findings, "scanning a conductor file must produce blocks"
    assert {item["path"] for item in findings} == {"conductor/radon_complexity.py"}


def test_a_grandfathered_symbol_that_worsens_fails(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    baseline = _baseline(tmp_path, [_finding(KEY, 21)])
    assert _run_check(baseline, [_finding(KEY, 25)], "D") == 1
    out = capsys.readouterr().out
    assert "blocks worsened" in out
    assert "(21 -> 25)" in out


def test_a_grandfathered_symbol_holding_its_score_passes(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    baseline = _baseline(tmp_path, [_finding(KEY, 21)])
    assert _run_check(baseline, [_finding(KEY, 21)], "D") == 0
    # The pass line names both failure modes, so assert on the failure phrase.
    assert "blocks worsened" not in capsys.readouterr().out


def test_an_improved_symbol_passes_and_asks_for_a_tighter_baseline(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    baseline = _baseline(tmp_path, [_finding(KEY, 21)])
    assert _run_check(baseline, [_finding(KEY, 15, rank="C")], "D") == 0
    out = capsys.readouterr().out
    # A block that fell below the minimum rank leaves the flagged set entirely;
    # the ratchet must still say so, or the baseline silently keeps the debt.
    assert "refresh-baseline" in out
    assert KEY in out


def test_a_new_block_above_the_minimum_rank_still_fails(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    baseline = _baseline(tmp_path, [])
    assert _run_check(baseline, [_finding(KEY, 25)], "D") == 1
    assert "new D-F blocks" in capsys.readouterr().out


def test_a_repeated_baseline_key_is_held_to_its_lowest_score(tmp_path: Path) -> None:
    # Two blocks can share path::name. Recording the larger of the two would
    # license the smaller one to grow up to it without the ratchet noticing.
    # This asserts the binding rule directly: re-running _run_check here would
    # only duplicate the worsening test above and cost that test its unique kill.
    baseline = _baseline(tmp_path, [_finding(KEY, 30), _finding(KEY, 21)])
    assert _load_baseline(baseline) == {KEY: 21}


def test_blocks_below_the_minimum_rank_are_not_ratcheted(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    baseline = _baseline(tmp_path, [])
    assert (
        _run_check(baseline, [_finding("conductor/example.py::small", 3, "A")], "D")
        == 0
    )
    assert "new D-F blocks" not in capsys.readouterr().out


def test_the_ratchet_runs_in_the_pre_commit_profile() -> None:
    # A ratchet nobody runs is a baseline file. pre-commit reviews with
    # --profile fast, so the check has to be declared for that profile or bad
    # code lands and is only found a branch later.
    policy = load_policy(REPO_ROOT / "conductor" / "candidate_policy.toml")
    complexity = [c for c in policy.checks if c.check_id == "complexity"]
    assert complexity, "candidate_policy.toml declares no complexity check"
    assert "fast" in complexity[0].profiles
    assert "full" in complexity[0].profiles
