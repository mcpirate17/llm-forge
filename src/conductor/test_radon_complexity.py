"""The complexity ratchet must bite on growth, not only on new symbols."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor.candidate_review.policy import load_policy
from conductor.candidate_review.policy_path import resolve_policy_path
from conductor.project_paths import package_relative, radon_baseline_path
from conductor.radon_complexity import (
    DEFAULT_PATHS,
    REPO_ROOT,
    _load_baseline,
    _resolve_baseline,
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
    # The ratchet lives in conductor and did not measure its own home. DEFAULT_PATHS
    # is the monorepo's own layout literal ("conductor" at the repo root); this repo
    # is src-layout, so the real file is found through the configured package_root.
    assert "conductor" in DEFAULT_PATHS
    own_module = (package_relative(REPO_ROOT) / "radon_complexity.py").as_posix()
    findings, parse_errors = _scan([own_module], [])
    assert parse_errors == []
    assert findings, "scanning a conductor file must produce blocks"
    assert {item["path"] for item in findings} == {own_module}


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
    # code lands and is only found a branch later. resolve_policy_path() is this
    # host's own policy resolver -- REPO_ROOT / "conductor" / "candidate_policy.toml"
    # is the monorepo's layout, not this src-layout repo's configured location.
    policy = load_policy(resolve_policy_path())
    complexity = [c for c in policy.checks if c.check_id == "complexity"]
    assert complexity, "candidate_policy.toml declares no complexity check"
    assert "fast" in complexity[0].profiles
    assert "full" in complexity[0].profiles


def test_the_unflagged_default_baseline_lives_in_the_host_tree() -> None:
    # Until 2026-09-15 the default was Path(__file__).parent / "...json" -- an
    # ABSOLUTE path -- and main() then did (REPO_ROOT / args.baseline), where an
    # absolute right operand discards REPO_ROOT entirely. Every consumer that ran
    # `python -m conductor.radon_complexity` therefore ratcheted its own tree
    # against this package's installed baseline. Measured on LLM the same day: a
    # 7,661-byte packaged baseline stood in for a 124,467-byte host one and the
    # check reported "405 new D-F blocks" against a tree that had regressed none.
    resolved = _resolve_baseline(None)
    assert resolved.is_relative_to(REPO_ROOT), resolved
    assert resolved == radon_baseline_path(REPO_ROOT).resolve()


def test_the_default_follows_the_host_and_not_this_package(tmp_path: Path) -> None:
    # The same package, asked about another host, must answer with that host's
    # file. This is the property the __file__-derived default could not have.
    (tmp_path / "pyproject.toml").write_text(
        '[tool.conductor]\nradon_complexity_baseline = "ratchet/base.json"\n',
        encoding="utf-8",
    )
    assert radon_baseline_path(tmp_path) == tmp_path / "ratchet" / "base.json"
    bare = tmp_path / "bare"
    bare.mkdir()
    assert (
        radon_baseline_path(bare)
        == bare / "conductor" / "radon_complexity_baseline.json"
    )


def test_a_relative_baseline_flag_resolves_against_the_host_root() -> None:
    assert _resolve_baseline("campaigns/x.json") == (REPO_ROOT / "campaigns/x.json")


def test_an_absolute_baseline_flag_is_taken_as_given(tmp_path: Path) -> None:
    # The caller named a file outside the host tree on purpose; joining it onto
    # REPO_ROOT would be a silent no-op, which is how the bug above hid.
    target = tmp_path / "elsewhere.json"
    assert _resolve_baseline(str(target)) == target.resolve()


def test_a_missing_baseline_is_refused_by_name(tmp_path: Path) -> None:
    # Fail loud: an unreadable baseline must never read as "nothing is
    # grandfathered", which would fail the ratchet on the entire tree.
    missing = tmp_path / "absent.json"
    with pytest.raises(FileNotFoundError) as excinfo:
        _load_baseline(missing)
    assert str(missing) in str(excinfo.value)
    assert "radon_complexity_baseline" in str(excinfo.value)
